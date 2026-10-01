//! Badges, not code: a declaration's signature read into a handful of small
//! typed tags you take in at a glance. What kind of thing it is, whether it
//! can fail, whether it borrows, what it is generic over, whether it is
//! async or unsafe. Nothing here ever prints `pub unsafe fn`: the signature
//! text is an *input*, the badges are the output, and each badge carries an
//! icon, one word and the sentence that says what the word means (the
//! sentence opens inside the badge when you rest on it).
//!
//! The grammar is Rust's in full (it is the language most packages here are
//! written in) and a smaller, honest reading of the other six ecosystems:
//! a word that cannot be read from the text is not invented, and a
//! signature that is not source text (an encoded compiler type) reads to no
//! badges at all.
//!
//! - [`read`] is the pure grammar: name, kind and signature in, [`Reading`]
//!   out.
//! - [`glyph`] paints the twelve-unit glyphs every badge and heads-up chip
//!   wears (cut, not drawn: square caps, mitred corners).
//! - [`view`] is the badge itself, an element that opens in place.

pub mod glyph;
mod other;
mod rust;
pub mod view;

pub use glyph::{Glyph, glyph};
pub use view::{Badge as BadgeView, badge};

use crate::icons::{Kind, Lang};
use crate::tokens::Family;
use gpui::SharedString;

/// What a badge's colour says. Plain is the neutral ink; the rest are the
/// house voices (amber waits and warns, coral fails, periwinkle is a
/// language idiom, teal is generic over a type, mint is yours).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Ink {
    /// Neutral.
    #[default]
    Plain,
    /// An idiom: `async`.
    Peri,
    /// Caution: `unsafe`, a C boundary.
    Amber,
    /// A normal outcome that is failure.
    Coral,
    /// Generic over a type.
    Teal,
    /// Yours; the safe side.
    Mint,
}

/// One badge: a glyph, one word, and the sentence behind the word.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Badge {
    /// The glyph it wears.
    pub glyph: Glyph,
    /// The one word.
    pub word: SharedString,
    /// What the word means, in a sentence.
    pub tip: SharedString,
    /// Its colour.
    pub ink: Ink,
}

impl Badge {
    pub(crate) fn new(
        glyph: Glyph,
        word: impl Into<SharedString>,
        tip: impl Into<SharedString>,
        ink: Ink,
    ) -> Self {
        Self {
            glyph,
            word: word.into(),
            tip: tip.into(),
            ink,
        }
    }
}

/// What a declaration is, in the reader's words rather than the compiler's.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Shape {
    /// A function or method.
    Function,
    /// A macro.
    Macro,
    /// A trait or interface: a contract.
    Contract,
    /// A type alias.
    Alias,
    /// A struct, class, record.
    Struct,
    /// An enum.
    Enum,
    /// A union.
    Union,
    /// A constant.
    Constant,
    /// A static.
    Static,
    /// A module or namespace.
    Module,
    /// Nothing more is known.
    Item,
}

impl Shape {
    /// The kind word every card leads with (`fn`, `struct`, `trait`).
    #[must_use]
    pub const fn word(self, lang: Lang) -> &'static str {
        match self {
            Self::Function => match lang {
                Lang::Rust => "fn",
                Lang::Go => "func",
                Lang::Python => "def",
                _ => "function",
            },
            Self::Macro => "macro",
            Self::Contract => match lang {
                Lang::Rust => "trait",
                _ => "interface",
            },
            Self::Alias => "alias",
            Self::Struct => match lang {
                Lang::Rust | Lang::Go => "struct",
                _ => "class",
            },
            Self::Enum => "enum",
            Self::Union => "union",
            Self::Constant => "const",
            Self::Static => "static",
            Self::Module => "module",
            Self::Item => "item",
        }
    }

    /// The family, which picks the hue of the card's mark.
    #[must_use]
    pub const fn family(self) -> Family {
        match self {
            Self::Function | Self::Macro => Family::Callable,
            Self::Contract => Family::Contract,
            Self::Alias | Self::Struct | Self::Enum | Self::Union => Family::Type,
            Self::Constant | Self::Static => Family::Value,
            Self::Module | Self::Item => Family::Namespace,
        }
    }

    /// The shape a kind implies before any signature is read.
    #[must_use]
    pub const fn of_kind(kind: Kind) -> Self {
        match kind {
            Kind::Module | Kind::Package | Kind::Import => Self::Module,
            Kind::Struct | Kind::Class => Self::Struct,
            Kind::Enum => Self::Enum,
            Kind::Union => Self::Union,
            Kind::Type => Self::Alias,
            Kind::Trait | Kind::Interface => Self::Contract,
            Kind::Function | Kind::Method | Kind::Constructor => Self::Function,
            Kind::Macro => Self::Macro,
            Kind::Constant | Kind::Field | Kind::Property | Kind::Variable | Kind::Variant => {
                Self::Constant
            }
            Kind::Unknown => Self::Item,
        }
    }
}

/// A declaration, as the outline gives it.
#[derive(Clone, Copy, Debug)]
pub struct Item<'a> {
    /// Its name.
    pub name: &'a str,
    /// The index's kind for it, when typed.
    pub kind: Option<Kind>,
    /// The signature text as the producer recorded it, when it captured one.
    pub signature: Option<&'a str>,
    /// The language it is written in.
    pub lang: Lang,
}

impl<'a> Item<'a> {
    /// An item of `lang` named `name`.
    #[must_use]
    pub const fn new(name: &'a str, lang: Lang) -> Self {
        Self {
            name,
            kind: None,
            signature: None,
            lang,
        }
    }

    /// With the index's kind.
    #[must_use]
    pub const fn kind(mut self, kind: Option<Kind>) -> Self {
        self.kind = kind;
        self
    }

    /// With its recorded signature text.
    #[must_use]
    pub const fn signature(mut self, signature: Option<&'a str>) -> Self {
        self.signature = signature;
        self
    }
}

/// What a signature reads to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reading {
    /// What it is.
    pub shape: Shape,
    /// The kind word (`fn`, `struct`, `trait`, ...).
    pub word: &'static str,
    /// Its badges, in reading order.
    pub badges: Vec<Badge>,
}

impl Reading {
    /// The words of every badge, in order.
    #[must_use]
    pub fn words(&self) -> Vec<&str> {
        self.badges
            .iter()
            .map(|badge| badge.word.as_ref())
            .collect()
    }

    /// Whether some badge says `word`.
    #[must_use]
    pub fn says(&self, word: &str) -> bool {
        self.badges.iter().any(|badge| badge.word == word)
    }
}

/// Reads an item into its shape and badges.
#[must_use]
pub fn read(item: &Item<'_>) -> Reading {
    let text = item.signature.map(normalize).filter(|text| !encoded(text));
    let hinted = item.kind.map(Shape::of_kind);
    let mut reading = match (&text, item.lang) {
        (Some(text), Lang::Rust) => rust::read(item, text),
        (Some(text), lang) => other::read(item, text, lang),
        (None, _) => Reading {
            shape: hinted.unwrap_or(Shape::Item),
            word: hinted.unwrap_or(Shape::Item).word(item.lang),
            badges: Vec::new(),
        },
    };
    // The index's kind outranks a guess from the text when they disagree
    // about the family (a `Function` row never reads as a struct).
    if let Some(hinted) = hinted
        && text.is_some()
        && reading.shape == Shape::Item
    {
        reading.shape = hinted;
        reading.word = hinted.word(item.lang);
    }
    reading
}

/// The producer served an encoded compiler type, not source text.
fn encoded(text: &str) -> bool {
    const HEADS: [&str; 30] = [
        "annotated(",
        "applied(",
        "array(",
        "builtin(",
        "c-qualified(",
        "channel(",
        "conditional(",
        "cxx-member-pointer(",
        "cxx-reference(",
        "entity(",
        "external(",
        "fixed(",
        "function(parameters=",
        "import(",
        "indexed(",
        "infer(",
        "inferred(",
        "map(",
        "mapped(",
        "namespace(",
        "nominal(",
        "package(",
        "pointer(",
        "qualified(",
        "rectangular(",
        "reference(",
        "tuple(",
        "typeof(",
        "unknown(",
        "literal.",
    ];
    HEADS.iter().any(|head| text.starts_with(head))
}

/// One line, one space between words, attributes and doc lines dropped.
pub(crate) fn normalize(signature: &str) -> String {
    let mut kept = String::new();
    let mut lines = signature
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .peekable();
    while let Some(line) = lines.next() {
        // `#[derive(..)]`, `#[inline]`, `@Override`, `[[nodiscard]]` stand alone on their line.
        let attribute = (line.starts_with("#[") && line.ends_with(']'))
            || (line.starts_with("#![") && line.ends_with(']'))
            || (line.starts_with("[[") && line.ends_with("]]"))
            || (line.starts_with('@') && !line.contains(' ') && !line.contains('('))
            || line.starts_with("///")
            || line.starts_with("//");
        if attribute && lines.peek().is_some() {
            continue;
        }
        if !kept.is_empty() {
            kept.push(' ');
        }
        kept.push_str(line);
    }
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text between the first `open` and its matching `close`, counting
/// nesting, and skipping the arrow `->` (and `=>`) so a closure's return
/// type never closes a generic list.
pub(crate) fn between(text: &str, open: char, close: char) -> Option<&str> {
    let start = text.find(open)?;
    let mut depth = 0_usize;
    let mut previous = '\0';
    for (offset, ch) in text[start..].char_indices() {
        if ch == open {
            depth += 1;
        } else if ch == close && !(close == '>' && (previous == '-' || previous == '=')) {
            depth -= 1;
            if depth == 0 {
                return Some(&text[start + open.len_utf8()..start + offset]);
            }
        }
        previous = ch;
    }
    Some(&text[start + open.len_utf8()..])
}

/// `text` cut at its commas that are not inside `()`, `[]`, `{}` or `<>`.
pub(crate) fn split_top(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut previous = '\0';
    let mut from = 0;
    for (offset, ch) in text.char_indices() {
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '>' if previous != '-' && previous != '=' => depth -= 1,
            ',' if depth <= 0 => {
                let part = text[from..offset].trim();
                if !part.is_empty() {
                    out.push(part);
                }
                from = offset + 1;
            }
            _ => {}
        }
        previous = ch;
    }
    let part = text[from..].trim();
    if !part.is_empty() {
        out.push(part);
    }
    out
}

/// The names of the type parameters in a generic list body, lifetimes left
/// out (`'a, T: Clone, const N: usize` reads `T N`).
pub(crate) fn type_params(body: &str) -> Vec<String> {
    split_top(body)
        .into_iter()
        .filter(|param| !param.starts_with('\''))
        .map(|param| {
            let name = param.split([':', '=']).next().unwrap_or(param).trim();
            name.strip_prefix("const ")
                .unwrap_or(name)
                .trim()
                .to_owned()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// A word made of the first `limit` names, `T U V +2` when there are more.
pub(crate) fn names_word(names: &[String], limit: usize) -> String {
    if names.len() <= limit {
        names.join(" ")
    } else {
        format!("{} +{}", names[..limit].join(" "), names.len() - limit)
    }
}

/// `text` without its final path (`std::io::Error` reads `Error`) or its
/// generic arguments (`Vec<T>` reads `Vec`).
pub(crate) fn last_name(text: &str) -> &str {
    let head = text.split('<').next().unwrap_or(text).trim();
    head.rsplit("::").next().unwrap_or(head).trim()
}

/// The identifier `text` starts with.
pub(crate) fn ident(text: &str) -> &str {
    let end = text
        .char_indices()
        .find(|(_, ch)| !(ch.is_alphanumeric() || *ch == '_' || *ch == '$'))
        .map_or(text.len(), |(at, _)| at);
    &text[..end]
}

/// Whether `word` appears in `text` as a whole word.
pub(crate) fn has_word(text: &str, word: &str) -> bool {
    find_word(text, word).is_some()
}

/// Where `word` first appears in `text` as a whole word.
pub(crate) fn find_word(text: &str, word: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(found) = text[from..].find(word) {
        let at = from + found;
        let end = at + word.len();
        let before = at == 0 || !is_word_byte(bytes[at - 1]);
        let after = end >= bytes.len() || !is_word_byte(bytes[end]);
        if before && after {
            return Some(at);
        }
        from = at + word.len().max(1);
    }
    None
}

const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

#[cfg(test)]
mod tests;
