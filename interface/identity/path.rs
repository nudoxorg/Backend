//! Defines path behavior for `interface-identity`, whose purpose is to spell, parse, and abbreviate every identity a person or agent can name.
//! This module owns the path invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The symbol path: root-to-leaf declaration names with optional readable kind qualifiers.

use core::fmt;

use compiler_ir_vocabulary::EntityKind;

/// Deepest declaration nesting any surface will spell.
pub const MAX_PATH_SEGMENTS: usize = 64;

/// One declaration name exactly as the semantic image spells it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SegmentName(Box<str>);

impl SegmentName {
    /// Returns the exact name text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One path step, optionally qualified by the declaration kind that disambiguates it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PathSegment {
    /// Exact declaration name.
    pub name: SegmentName,
    /// Kind qualifier, rendered as `[fn]`, `[struct]`, and so on.
    pub kind: Option<EntityKind>,
}

impl PathSegment {
    /// Builds a segment from a non-empty name.
    #[must_use]
    pub fn new(name: &str, kind: Option<EntityKind>) -> Option<Self> {
        (!name.is_empty()).then(|| Self {
            name: SegmentName(name.into()),
            kind,
        })
    }
}

/// Root-to-leaf declaration path inside one package.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SymbolPath(Box<[PathSegment]>);

impl SymbolPath {
    /// Builds a path from ordered segments.
    ///
    /// # Errors
    ///
    /// Rejects a path deeper than [`MAX_PATH_SEGMENTS`].
    pub fn new(segments: Vec<PathSegment>) -> Result<Self, PathParseError> {
        if segments.len() > MAX_PATH_SEGMENTS {
            return Err(PathParseError::TooDeep {
                observed: segments.len(),
                maximum: MAX_PATH_SEGMENTS,
            });
        }
        Ok(Self(segments.into_boxed_slice()))
    }

    /// Returns the ordered segments.
    #[must_use]
    pub fn segments(&self) -> &[PathSegment] {
        &self.0
    }

    /// Returns the leaf segment when the path is non-empty.
    #[must_use]
    pub fn leaf(&self) -> Option<&PathSegment> {
        self.0.last()
    }

    /// Whether the path names the package root itself.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Parses `a::b::c[kind]`, splitting on `::` only outside angle brackets so an `impl` segment
    /// such as `impl Display for Vec<std::string::String>` stays whole.
    ///
    /// # Errors
    ///
    /// Returns the exact segment that was empty or the depth that was exceeded.
    pub fn parse(text: &str) -> Result<Self, PathParseError> {
        if text.is_empty() {
            return Ok(Self::default());
        }
        let mut segments = Vec::new();
        for (index, raw) in split_top_level(text).enumerate() {
            if raw.is_empty() {
                return Err(PathParseError::EmptySegment { index });
            }
            let (name, kind) = split_kind_suffix(raw);
            let Some(segment) = PathSegment::new(name, kind) else {
                return Err(PathParseError::EmptySegment { index });
            };
            segments.push(segment);
            if segments.len() > MAX_PATH_SEGMENTS {
                return Err(PathParseError::TooDeep {
                    observed: segments.len(),
                    maximum: MAX_PATH_SEGMENTS,
                });
            }
        }
        Ok(Self(segments.into_boxed_slice()))
    }
}

impl fmt::Display for SymbolPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index != 0 {
                formatter.write_str("::")?;
            }
            formatter.write_str(&segment.name.0)?;
            if let Some(kind) = segment.kind {
                write!(formatter, "[{}]", KindTag::of(kind).as_str())?;
            }
        }
        Ok(())
    }
}

/// Splits on `::` at angle-bracket and parenthesis depth zero.
///
/// Cuts land on ASCII colons, so every recorded range is a character boundary. A range that is
/// somehow not representable yields the empty string, which the caller rejects as an empty
/// segment; a segment is never silently dropped.
fn split_top_level(text: &str) -> impl Iterator<Item = &str> {
    let mut depth = 0_u32;
    let mut start = 0_usize;
    let mut pending_colon: Option<usize> = None;
    let mut cuts = Vec::new();
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'<' | b'(' | b'[' => {
                depth = depth.saturating_add(1);
                pending_colon = None;
            }
            b'>' | b')' | b']' => {
                depth = depth.saturating_sub(1);
                pending_colon = None;
            }
            b':' if depth == 0 => match pending_colon.take() {
                Some(first) => {
                    cuts.push((start, first));
                    start = index.saturating_add(1);
                }
                None => pending_colon = Some(index),
            },
            _ => pending_colon = None,
        }
    }
    cuts.push((start, text.len()));
    cuts.into_iter()
        .map(move |(from, to)| text.get(from..to).unwrap_or_default())
}

/// Peels a trailing `[kind]` qualifier only when the bracketed text is a known kind tag.
fn split_kind_suffix(raw: &str) -> (&str, Option<EntityKind>) {
    let Some(stripped) = raw.strip_suffix(']') else {
        return (raw, None);
    };
    let Some(open) = stripped.rfind('[') else {
        return (raw, None);
    };
    let (Some(name), Some(tag)) = (stripped.get(..open), stripped.get(open.saturating_add(1)..))
    else {
        return (raw, None);
    };
    match KindTag::parse(tag) {
        Some(kind) => (name, Some(kind)),
        None => (raw, None),
    }
}

/// Short readable kind tag used inside path qualifiers, badges, and search filters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KindTag(&'static str);

impl KindTag {
    /// Returns the tag for one declaration kind.
    #[must_use]
    pub const fn of(kind: EntityKind) -> Self {
        Self(match kind {
            EntityKind::Function => "fn",
            EntityKind::Constant => "const",
            EntityKind::Record => "struct",
            EntityKind::Module => "mod",
            EntityKind::Field => "field",
            EntityKind::Alias => "type",
            EntityKind::Trait => "trait",
            EntityKind::Implementation => "impl",
            EntityKind::Enum => "enum",
            EntityKind::Variant => "variant",
            EntityKind::Static => "static",
            EntityKind::Reexport => "use",
            EntityKind::Parameter => "param",
            EntityKind::Macro => "macro",
            EntityKind::Namespace => "ns",
        })
    }

    /// Returns the exact tag text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    /// One-character glyph every surface draws beside a declaration of this kind.
    ///
    /// Shared here so the CLI, the MCP Markdown, and the GUI badge agree; a kind's glyph is part
    /// of its identity, not a renderer's choice.
    #[must_use]
    pub const fn glyph(kind: EntityKind) -> &'static str {
        match kind {
            EntityKind::Function => "ƒ",
            EntityKind::Constant => "π",
            EntityKind::Record => "▣",
            EntityKind::Module => "▤",
            EntityKind::Field => "·",
            EntityKind::Alias => "≡",
            EntityKind::Trait => "◇",
            EntityKind::Implementation => "◆",
            EntityKind::Enum => "⊞",
            EntityKind::Variant => "⊟",
            EntityKind::Static => "◉",
            EntityKind::Reexport => "↪",
            EntityKind::Parameter => "∘",
            EntityKind::Macro => "!",
            EntityKind::Namespace => "⁝",
        }
    }

    /// Parses a tag, accepting the readable spelling and the canonical kind name.
    #[must_use]
    pub fn parse(text: &str) -> Option<EntityKind> {
        EntityKind::ALL
            .into_iter()
            .find(|kind| Self::of(*kind).0 == text || canonical_name(*kind).eq_ignore_ascii_case(text))
    }
}

const fn canonical_name(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Function => "Function",
        EntityKind::Constant => "Constant",
        EntityKind::Record => "Record",
        EntityKind::Module => "Module",
        EntityKind::Field => "Field",
        EntityKind::Alias => "Alias",
        EntityKind::Trait => "Trait",
        EntityKind::Implementation => "Implementation",
        EntityKind::Enum => "Enum",
        EntityKind::Variant => "Variant",
        EntityKind::Static => "Static",
        EntityKind::Reexport => "Reexport",
        EntityKind::Parameter => "Parameter",
        EntityKind::Macro => "Macro",
        EntityKind::Namespace => "Namespace",
    }
}

/// Exact path spelling rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathParseError {
    /// A `::`-delimited segment was empty.
    EmptySegment {
        /// Zero-based segment position.
        index: usize,
    },
    /// The path nests deeper than the fixed budget.
    TooDeep {
        /// Observed segment count.
        observed: usize,
        /// Accepted segment count.
        maximum: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impl_segments_survive_nested_paths_and_kind_tags_peel() -> Result<(), PathParseError> {
        let text = "serde::de::impl Display for Vec<std::string::String>::fmt[fn]";
        let path = SymbolPath::parse(text)?;
        let names: Vec<&str> = path
            .segments()
            .iter()
            .map(|segment| segment.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "serde",
                "de",
                "impl Display for Vec<std::string::String>",
                "fmt"
            ]
        );
        assert_eq!(
            path.leaf().map(|segment| segment.kind),
            Some(Some(EntityKind::Function))
        );
        assert_eq!(path.to_string(), text);
        assert_eq!(
            SymbolPath::parse("a::::b"),
            Err(PathParseError::EmptySegment { index: 1 })
        );
        let literal = SymbolPath::parse("data[0]")?;
        assert_eq!(
            literal.leaf().map(|segment| segment.name.as_str()),
            Some("data[0]")
        );
        Ok(())
    }
}
