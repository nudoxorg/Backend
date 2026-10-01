//! Declaration facts a reader weighs before using a declaration: whether it
//! is deprecated, and what an implementor of its contract owes for it.
//!
//! Two producers fill this one carrier. The structural lane reads the
//! tree-sitter parse; the semantic lane reads the attributes and
//! documentation its compiler authority retained. Both call the same
//! interpreters below, so the two lanes cannot disagree about how one
//! attribute spelling reads.
//!
//! A fact a producer did not look for is [`Fact::Unobserved`], never
//! [`Fact::Absent`]: "this lane cannot see deprecation for C++" and "this
//! function is not deprecated" are different statements, and a page must be
//! able to tell them apart.

use crate::SourceLanguage;

/// Maximum UTF-8 bytes retained for one fact text (a `since` or a note).
pub const MAX_FACT_TEXT_BYTES: usize = 1024;

/// One fact as a producer observed it.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub enum Fact<T> {
    /// The producer did not look for this fact.
    #[default]
    Unobserved,
    /// The producer looked, and the declaration states nothing.
    Absent,
    /// The producer looked and found this.
    Present(T),
}

impl<T> Fact<T> {
    /// An observation from an optional finding.
    #[must_use]
    pub fn observed(value: Option<T>) -> Self {
        value.map_or(Self::Absent, Self::Present)
    }

    /// Returns whether the producer looked.
    #[must_use]
    pub const fn is_observed(&self) -> bool {
        !matches!(self, Self::Unobserved)
    }

    /// Returns the finding, when there is one.
    #[must_use]
    pub const fn present(&self) -> Option<&T> {
        match self {
            Self::Present(value) => Some(value),
            Self::Unobserved | Self::Absent => None,
        }
    }

    /// Keeps this observation, or takes `fallback` when this one never looked.
    #[must_use]
    pub fn or(self, fallback: Self) -> Self {
        if self.is_observed() { self } else { fallback }
    }
}

/// A deprecation notice as the source states it.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct Deprecation {
    since: Option<Box<str>>,
    note: Option<Box<str>>,
}

/// A fact text that cannot be admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactError {
    /// One text exceeded [`MAX_FACT_TEXT_BYTES`].
    Oversized {
        /// Bytes supplied.
        actual: usize,
    },
    /// A tag byte named no known value.
    UnknownTag(u8),
}

impl core::fmt::Display for FactError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Oversized { actual } => write!(
                formatter,
                "declaration fact text has {actual} bytes, maximum is {MAX_FACT_TEXT_BYTES}"
            ),
            Self::UnknownTag(tag) => write!(formatter, "unknown declaration fact tag {tag}"),
        }
    }
}

impl std::error::Error for FactError {}

impl Deprecation {
    /// Builds a notice from producer text. Blank parts become absent, and a
    /// part longer than [`MAX_FACT_TEXT_BYTES`] is cut on a character
    /// boundary and marked with `…`, never silently.
    #[must_use]
    pub fn new(since: Option<&str>, note: Option<&str>) -> Self {
        Self {
            since: since.and_then(bounded),
            note: note.and_then(bounded),
        }
    }

    /// Admits an already-bounded notice read back from storage or the wire.
    ///
    /// # Errors
    /// Returns [`FactError::Oversized`] when a part exceeds the bound.
    pub fn admit(since: Option<String>, note: Option<String>) -> Result<Self, FactError> {
        for part in [since.as_deref(), note.as_deref()].into_iter().flatten() {
            if part.len() > MAX_FACT_TEXT_BYTES {
                return Err(FactError::Oversized { actual: part.len() });
            }
        }
        Ok(Self {
            since: since.map(String::into_boxed_str),
            note: note.map(String::into_boxed_str),
        })
    }

    /// The version the source says the deprecation began in.
    #[must_use]
    pub fn since(&self) -> Option<&str> {
        self.since.as_deref()
    }

    /// The source's own words about the deprecation.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Bytes of retained text.
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        self.since.as_deref().map_or(0, str::len) + self.note.as_deref().map_or(0, str::len)
    }

    /// Keeps this notice's own parts and fills the missing ones from `other`.
    ///
    /// Java writes the version on `@Deprecated(since = …)` and the words in
    /// the `Javadoc` `@deprecated` tag; one notice is both.
    #[must_use]
    pub fn completed_by(self, other: Option<&Self>) -> Self {
        let Some(other) = other else {
            return self;
        };
        Self {
            since: self.since.or_else(|| other.since.clone()),
            note: self.note.or_else(|| other.note.clone()),
        }
    }
}

/// What an implementor of the enclosing contract owes for one member.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Obligation {
    /// Implementors must write it: a Rust trait method without a body, a
    /// Java/C#/TypeScript `abstract` member, a Python `@abstractmethod`, a Go
    /// interface method, a C++ pure virtual function.
    Required = 1,
    /// Implementors may leave it out: a TypeScript `m?()` or `p?: T` member.
    Optional = 2,
    /// It comes with an implementation implementors get without writing it:
    /// a Rust default method, a Java `default` method, a C# default or
    /// `virtual` member, a C++ virtual function with a body, a concrete
    /// method of an abstract class.
    Provided = 3,
}

impl Obligation {
    /// Returns the stable lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
            Self::Provided => "provided",
        }
    }

    /// Returns the stable canonical tag.
    #[must_use]
    pub const fn wire_tag(self) -> u8 {
        self as u8
    }

    /// Admits a canonical tag.
    ///
    /// # Errors
    /// Returns [`FactError::UnknownTag`] for a tag no obligation owns.
    pub const fn from_wire_tag(tag: u8) -> Result<Self, FactError> {
        match tag {
            1 => Ok(Self::Required),
            2 => Ok(Self::Optional),
            3 => Ok(Self::Provided),
            other => Err(FactError::UnknownTag(other)),
        }
    }

    /// Admits a stable name.
    ///
    /// # Errors
    /// Returns [`FactError::UnknownTag`] (with tag 0) for an unknown name.
    pub fn from_name(name: &str) -> Result<Self, FactError> {
        match name {
            "required" => Ok(Self::Required),
            "optional" => Ok(Self::Optional),
            "provided" => Ok(Self::Provided),
            _ => Err(FactError::UnknownTag(0)),
        }
    }
}

/// Everything a producer observed about one declaration beyond its name,
/// kind, signature, and documentation.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct DeclarationFacts {
    /// Whether and how the declaration is deprecated.
    pub deprecation: Fact<Deprecation>,
    /// What an implementor of the enclosing contract owes for it.
    pub obligation: Fact<Obligation>,
}

impl DeclarationFacts {
    /// Facts from a producer that looked for nothing.
    pub const UNOBSERVED: Self = Self {
        deprecation: Fact::Unobserved,
        obligation: Fact::Unobserved,
    };

    /// Returns whether the producer looked for nothing at all.
    #[must_use]
    pub const fn is_unobserved(&self) -> bool {
        !self.deprecation.is_observed() && !self.obligation.is_observed()
    }

    /// Keeps each fact this producer observed and takes the rest from
    /// `fallback`. The semantic lane is merged over its paired structural
    /// declaration this way.
    #[must_use]
    pub fn or(self, fallback: &Self) -> Self {
        Self {
            deprecation: self.deprecation.or(fallback.deprecation.clone()),
            obligation: self.obligation.or(fallback.obligation.clone()),
        }
    }

    /// Bytes of retained text, for encoded-size budgets.
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        self.deprecation
            .present()
            .map_or(0, Deprecation::text_bytes)
    }
}

/// Reads a deprecation from one attribute, annotation, or decorator exactly
/// as written, with or without its sigil (`#[…]`, `@…`, `[…]`, `[[…]]`,
/// `__attribute__((…))`). A list (`[Serializable, Obsolete("…")]`) is read
/// item by item. Returns `None` when the spelling states no deprecation.
#[must_use]
pub fn deprecation_in_attribute(language: SourceLanguage, text: &str) -> Option<Deprecation> {
    let body = unwrap_attribute(text.trim());
    split_top_level(body, b',')
        .into_iter()
        .find_map(|item| deprecation_in_item(language, item.trim()))
}

/// Reads a deprecation from documentation text, lines separated by `\n`.
///
/// * `TypeScript` and `JavaScript` (`JSDoc`), Java (`Javadoc`), C and C++ (`Doxygen`): a
///   `@deprecated` or `\deprecated` tag; its words run to the next tag or
///   blank line.
/// * Go: a paragraph that begins `Deprecated:`.
/// * Python: a `.. deprecated:: <version>` directive and its indented body.
/// * Rust and C# say it only with an attribute, so their documentation is
///   never read for it.
#[must_use]
pub fn deprecation_in_documentation(language: SourceLanguage, text: &str) -> Option<Deprecation> {
    let lines: Vec<&str> = text.lines().collect();
    match language {
        SourceLanguage::TypeScript | SourceLanguage::Java | SourceLanguage::Clang => {
            tagged_deprecation(&lines)
        }
        SourceLanguage::Go => go_deprecation(&lines),
        SourceLanguage::Python => sphinx_deprecation(&lines),
        SourceLanguage::Rust | SourceLanguage::CSharp => None,
    }
}

/// Returns whether one Python decorator, as written, marks an abstract
/// method (`abstractmethod`, `abc.abstractmethod`, with or without `@`).
#[must_use]
pub fn is_abstract_method_decorator(text: &str) -> bool {
    let text = text.trim().trim_start_matches('@').trim();
    let path = text.split('(').next().unwrap_or(text).trim();
    matches!(path, "abstractmethod" | "abc.abstractmethod")
}

fn bounded(text: &str) -> Option<Box<str>> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.len() <= MAX_FACT_TEXT_BYTES {
        return Some(Box::from(text));
    }
    let mut end = MAX_FACT_TEXT_BYTES - '…'.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(Box::from(format!("{}…", &text[..end])))
}

/// Strips one attribute's sigil and brackets, leaving its items.
fn unwrap_attribute(text: &str) -> &str {
    let text = text.trim();
    for (open, close) in [
        ("#![", "]"),
        ("#[", "]"),
        ("[[", "]]"),
        ("__attribute__((", "))"),
        ("__declspec(", ")"),
        ("[", "]"),
    ] {
        if let Some(inner) = text.strip_prefix(open) {
            let inner = inner.trim_end();
            return inner.strip_suffix(close).unwrap_or(inner).trim();
        }
    }
    text.strip_prefix('@').unwrap_or(text).trim()
}

/// One attribute item: its path and, when written, its argument list or
/// `= value`.
struct Item<'a> {
    path: &'a str,
    arguments: Vec<Argument>,
}

/// One argument: an optional `key =` and its value, with string literals
/// already unquoted.
struct Argument {
    key: Option<String>,
    value: String,
    literal: bool,
}

fn parse_item(text: &str) -> Option<Item<'_>> {
    let text = text.trim();
    // A C# attribute may name its target: `[method: Obsolete("…")]`.
    let text = match text.split_once(':') {
        Some((target, rest))
            if !target.contains(['(', '"', '\''])
                && !rest.starts_with(':')
                && !target.ends_with(':')
                && target.chars().all(|c| c.is_ascii_alphabetic() || c == ' ') =>
        {
            rest.trim()
        }
        _ => text,
    };
    let path_end = text
        .find(|c: char| c == '(' || c == '=' || c.is_whitespace())
        .unwrap_or(text.len());
    let path = text[..path_end].trim();
    if path.is_empty() {
        return None;
    }
    let rest = text[path_end..].trim();
    let arguments = if let Some(inner) = rest.strip_prefix('(') {
        let inner = inner.trim_end();
        let inner = inner.strip_suffix(')').unwrap_or(inner);
        split_top_level(inner, b',')
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .map(parse_argument)
            .collect()
    } else if let Some(value) = rest.strip_prefix('=') {
        vec![parse_argument(value)]
    } else {
        Vec::new()
    };
    Some(Item { path, arguments })
}

fn parse_argument(text: &str) -> Argument {
    let text = text.trim();
    let (key, value) = match split_top_level(text, b'=').as_slice() {
        [key, value]
            if !key.trim().is_empty()
                && key.trim().chars().all(|c| c.is_alphanumeric() || c == '_') =>
        {
            (Some(key.trim().to_owned()), value.trim())
        }
        _ => (None, text),
    };
    match unquote(value) {
        Some(text) => Argument {
            key,
            value: text,
            literal: true,
        },
        None => Argument {
            key,
            value: value.to_owned(),
            literal: false,
        },
    }
}

/// Unquotes one string literal, or a run of adjacent literals (`"a" "b"`),
/// in the spellings the supported languages write: `"…"`, `'…'`, raw
/// `r"…"`/`r#"…"#`, and C# verbatim `@"…"`. Returns `None` for anything
/// that is not a string literal.
fn unquote(text: &str) -> Option<String> {
    let mut rest = text.trim();
    let mut out = String::new();
    let mut any = false;
    while !rest.is_empty() {
        let (value, after) = one_literal(rest)?;
        out.push_str(&value);
        any = true;
        rest = after.trim_start();
    }
    any.then_some(out)
}

fn one_literal(text: &str) -> Option<(String, &str)> {
    let bytes = text.as_bytes();
    // Raw Rust strings: r"…", r#"…"#.
    if bytes.first() == Some(&b'r') {
        let hashes = bytes[1..].iter().take_while(|byte| **byte == b'#').count();
        if bytes.get(1 + hashes) == Some(&b'"') {
            let body = &text[2 + hashes..];
            let closer = format!("\"{}", "#".repeat(hashes));
            let end = body.find(&closer)?;
            return Some((body[..end].to_owned(), &body[end + closer.len()..]));
        }
    }
    // C# verbatim strings: @"…" with "" as an escaped quote.
    if let Some(body) = text.strip_prefix("@\"") {
        let mut out = String::new();
        let mut chars = body.char_indices().peekable();
        while let Some((index, c)) = chars.next() {
            if c == '"' {
                if chars.peek().is_some_and(|(_, next)| *next == '"') {
                    out.push('"');
                    chars.next();
                    continue;
                }
                return Some((out, &body[index + 1..]));
            }
            out.push(c);
        }
        return None;
    }
    // Python prefixes (u, b, f are all plain for our purpose).
    let text = text.trim_start_matches(['u', 'U', 'b', 'B', 'f', 'F']);
    let quote = text.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let triple: String = std::iter::repeat_n(quote, 3).collect();
    if let Some(body) = text.strip_prefix(&triple) {
        let end = body.find(&triple)?;
        return Some((body[..end].to_owned(), &body[end + 3..]));
    }
    let body = &text[1..];
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '\\' => {
                if let Some((_, escaped)) = chars.next() {
                    out.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                }
            }
            c if c == quote => return Some((out, &body[index + 1..])),
            c => out.push(c),
        }
    }
    None
}

/// Splits on `separator` outside parentheses, brackets, and string literals.
fn split_top_level(text: &str, separator: u8) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let mut start = 0;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == open {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ if byte == separator && depth == 0 => {
                // `==`, `<=`, `>=`, `!=` are not argument separators.
                let doubled = separator == b'='
                    && (bytes.get(index + 1) == Some(&b'=')
                        || index
                            .checked_sub(1)
                            .and_then(|previous| bytes.get(previous))
                            .is_some_and(|previous| matches!(previous, b'=' | b'<' | b'>' | b'!')));
                if !doubled {
                    parts.push(&text[start..index]);
                    start = index + 1;
                }
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn last_segment(path: &str) -> &str {
    path.rsplit(['.', ':']).next().unwrap_or(path)
}

fn keyed<'a>(arguments: &'a [Argument], keys: &[&str]) -> Option<&'a str> {
    arguments
        .iter()
        .find(|argument| {
            argument.literal
                && argument
                    .key
                    .as_deref()
                    .is_some_and(|key| keys.contains(&key))
        })
        .map(|argument| argument.value.as_str())
}

fn first_positional(arguments: &[Argument]) -> Option<&str> {
    arguments
        .first()
        .filter(|argument| argument.key.is_none() && argument.literal)
        .map(|argument| argument.value.as_str())
}

fn deprecation_in_item(language: SourceLanguage, text: &str) -> Option<Deprecation> {
    let item = parse_item(text)?;
    let last = last_segment(item.path);
    match language {
        SourceLanguage::Rust => (item.path == "deprecated").then(|| {
            let bare = item
                .arguments
                .first()
                .filter(|argument| argument.key.is_none())
                .map(|argument| argument.value.as_str());
            Deprecation::new(
                keyed(&item.arguments, &["since"]),
                keyed(&item.arguments, &["note"]).or(bare),
            )
        }),
        SourceLanguage::Java => (last == "Deprecated")
            .then(|| Deprecation::new(keyed(&item.arguments, &["since"]), None)),
        SourceLanguage::CSharp => matches!(last, "Obsolete" | "ObsoleteAttribute").then(|| {
            Deprecation::new(
                None,
                first_positional(&item.arguments).or(keyed(&item.arguments, &["message"])),
            )
        }),
        SourceLanguage::Python => (last == "deprecated").then(|| {
            Deprecation::new(
                keyed(&item.arguments, &["version", "deprecated_in"]),
                first_positional(&item.arguments).or(keyed(
                    &item.arguments,
                    &["reason", "details", "message", "msg"],
                )),
            )
        }),
        SourceLanguage::Clang => (last == "deprecated").then(|| {
            Deprecation::new(
                None,
                first_positional(&item.arguments).or(keyed(&item.arguments, &["message"])),
            )
        }),
        SourceLanguage::TypeScript | SourceLanguage::Go => None,
    }
}

fn tagged_deprecation(lines: &[&str]) -> Option<Deprecation> {
    let start = lines.iter().position(|line| {
        let line = line.trim();
        line.strip_prefix("@deprecated")
            .or_else(|| line.strip_prefix("\\deprecated"))
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    })?;
    let first = lines[start].trim();
    let mut words = vec![
        first
            .trim_start_matches("@deprecated")
            .trim_start_matches("\\deprecated")
            .trim(),
    ];
    for line in &lines[start + 1..] {
        let line = line.trim();
        if line.is_empty() || line.starts_with('@') || line.starts_with('\\') {
            break;
        }
        words.push(line);
    }
    let note = words.join(" ");
    Some(Deprecation::new(None, Some(&note)))
}

fn go_deprecation(lines: &[&str]) -> Option<Deprecation> {
    let start = lines
        .iter()
        .position(|line| line.trim_start().starts_with("Deprecated:"))?;
    // The convention is a paragraph of its own.
    if start > 0 && !lines[start - 1].trim().is_empty() {
        return None;
    }
    let mut words = vec![
        lines[start]
            .trim_start()
            .trim_start_matches("Deprecated:")
            .trim(),
    ];
    for line in &lines[start + 1..] {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        words.push(line);
    }
    Some(Deprecation::new(None, Some(&words.join(" "))))
}

fn sphinx_deprecation(lines: &[&str]) -> Option<Deprecation> {
    let start = lines
        .iter()
        .position(|line| line.trim_start().starts_with(".. deprecated::"))?;
    let version = lines[start]
        .trim_start()
        .trim_start_matches(".. deprecated::")
        .trim();
    let mut words = Vec::new();
    for line in &lines[start + 1..] {
        if line.trim().is_empty() {
            if words.is_empty() {
                continue;
            }
            break;
        }
        if !line.starts_with(char::is_whitespace) {
            break;
        }
        words.push(line.trim());
    }
    let note = words.join(" ");
    Some(Deprecation::new(Some(version), Some(&note)))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn deprecated(language: SourceLanguage, text: &str) -> (Option<String>, Option<String>) {
        let notice = deprecation_in_attribute(language, text).expect("a deprecation");
        (
            notice.since().map(str::to_owned),
            notice.note().map(str::to_owned),
        )
    }

    fn some(since: Option<&str>, note: Option<&str>) -> (Option<String>, Option<String>) {
        (since.map(str::to_owned), note.map(str::to_owned))
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one assertion per spelling, read top to bottom
    fn every_attribute_spelling_reads_its_since_and_note() {
        assert_eq!(
            deprecated(
                SourceLanguage::Rust,
                r#"#[deprecated(since = "1.2.0", note = "use `fresh`")]"#
            ),
            some(Some("1.2.0"), Some("use `fresh`"))
        );
        assert_eq!(
            deprecated(SourceLanguage::Rust, r#"deprecated = "gone""#),
            some(None, Some("gone"))
        );
        assert_eq!(
            deprecated(SourceLanguage::Rust, "#[deprecated]"),
            some(None, None)
        );
        assert_eq!(
            deprecated(
                SourceLanguage::Java,
                r#"@java.lang.Deprecated(since="9", forRemoval=true)"#
            ),
            some(Some("9"), None)
        );
        assert_eq!(
            deprecated(SourceLanguage::Java, "@Deprecated"),
            some(None, None)
        );
        assert_eq!(
            deprecated(
                SourceLanguage::CSharp,
                r#"[Serializable, Obsolete("legacy", true)]"#
            ),
            some(None, Some("legacy"))
        );
        assert_eq!(
            deprecated(SourceLanguage::CSharp, r#"Obsolete("use New")"#),
            some(None, Some("use New"))
        );
        assert_eq!(
            deprecated(
                SourceLanguage::CSharp,
                r#"[method: System.Obsolete(@"say ""no""")]"#
            ),
            some(None, Some(r#"say "no""#))
        );
        assert_eq!(
            deprecated(SourceLanguage::Python, r#"@deprecated("use g")"#),
            some(None, Some("use g"))
        );
        assert_eq!(
            deprecated(
                SourceLanguage::Python,
                r"typing_extensions.deprecated('use g', category=None)"
            ),
            some(None, Some("use g"))
        );
        assert_eq!(
            deprecated(
                SourceLanguage::Clang,
                r#"[[nodiscard, deprecated("use g" " instead")]]"#
            ),
            some(None, Some("use g instead"))
        );
        assert_eq!(
            deprecated(
                SourceLanguage::Clang,
                r#"__attribute__((deprecated("old")))"#
            ),
            some(None, Some("old"))
        );
        for (language, text) in [
            (SourceLanguage::Rust, "#[derive(Clone)]"),
            (SourceLanguage::Rust, r#"#[doc = "deprecated"]"#),
            (SourceLanguage::Java, "@Override"),
            (SourceLanguage::CSharp, "[Serializable]"),
            (SourceLanguage::Python, "@abstractmethod"),
            (SourceLanguage::Clang, "[[nodiscard]]"),
        ] {
            assert_eq!(deprecation_in_attribute(language, text), None, "{text}");
        }
    }

    #[test]
    fn documentation_conventions_read_their_words() {
        let jsdoc = "Builds one.\n@deprecated Use `build`\ninstead.\n@param x the input";
        assert_eq!(
            deprecation_in_documentation(SourceLanguage::TypeScript, jsdoc)
                .and_then(|notice| notice.note().map(str::to_owned)),
            Some("Use `build` instead.".to_owned())
        );
        let go = "Old returns one.\n\nDeprecated: use New.\nIt is faster.\n\nMore.";
        assert_eq!(
            deprecation_in_documentation(SourceLanguage::Go, go)
                .and_then(|notice| notice.note().map(str::to_owned)),
            Some("use New. It is faster.".to_owned())
        );
        assert_eq!(
            deprecation_in_documentation(SourceLanguage::Go, "Says Deprecated: inline."),
            None
        );
        let sphinx = "Old.\n\n.. deprecated:: 3.1\n   Use :func:`g`.\n";
        let notice = deprecation_in_documentation(SourceLanguage::Python, sphinx).expect("sphinx");
        assert_eq!(notice.since(), Some("3.1"));
        assert_eq!(notice.note(), Some("Use :func:`g`."));
        assert_eq!(
            deprecation_in_documentation(SourceLanguage::Rust, "@deprecated no"),
            None
        );
    }

    #[test]
    fn an_oversized_note_is_cut_visibly_and_refused_on_admission() {
        let long = "x".repeat(MAX_FACT_TEXT_BYTES + 10);
        let notice = Deprecation::new(None, Some(&long));
        let note = notice.note().expect("note");
        assert!(note.len() <= MAX_FACT_TEXT_BYTES);
        assert!(note.ends_with('…'));
        assert_eq!(
            Deprecation::admit(None, Some(long)),
            Err(FactError::Oversized {
                actual: MAX_FACT_TEXT_BYTES + 10
            })
        );
    }

    #[test]
    fn a_semantic_observation_wins_only_when_it_looked() {
        let site = DeclarationFacts {
            deprecation: Fact::Present(Deprecation::new(None, Some("site"))),
            obligation: Fact::Present(Obligation::Required),
        };
        let semantic = DeclarationFacts {
            deprecation: Fact::Absent,
            obligation: Fact::Unobserved,
        };
        let merged = semantic.or(&site);
        assert_eq!(merged.deprecation, Fact::Absent);
        assert_eq!(merged.obligation, Fact::Present(Obligation::Required));
        assert!(DeclarationFacts::UNOBSERVED.is_unobserved());
    }
}
