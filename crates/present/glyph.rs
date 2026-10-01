//! The one glyph table and relation vocabulary shared by every surface.
//!
//! A glyph is a single display character, chosen so an outline tree reads as a
//! shape rather than a wall of words. There is exactly one table: if the CLI
//! and the MCP text rendering ever disagree about what a trait looks like, it
//! is because someone added a second table, which this module exists to
//! prevent.

use backend_library::{DeclarationKind, SemanticLinkKind};
use core::fmt;

use crate::language::Language;

/// The display glyph for one declaration kind.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KindGlyph(DeclarationKind);

impl KindGlyph {
    /// Selects the glyph for one declaration kind.
    #[must_use]
    pub const fn new(kind: DeclarationKind) -> Self {
        Self(kind)
    }

    /// Returns the single display character.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self.0 {
            DeclarationKind::Module => "▦",
            DeclarationKind::Class => "◆",
            DeclarationKind::Function | DeclarationKind::Method => "ƒ",
            DeclarationKind::Interface | DeclarationKind::Trait => "◇",
            DeclarationKind::Type => "τ",
            DeclarationKind::Macro => "!",
            DeclarationKind::Constant => "▪",
            DeclarationKind::Field | DeclarationKind::Property => "·",
            DeclarationKind::Constructor => "✦",
            DeclarationKind::Enum => "⊞",
            DeclarationKind::Struct => "▣",
            DeclarationKind::Union => "⊔",
            DeclarationKind::Variable => "▫",
            DeclarationKind::Import => "→",
            DeclarationKind::Variant => "◦",
            DeclarationKind::Unknown => "?",
        }
    }

    /// Returns the declaration kind this glyph stands for.
    #[must_use]
    pub const fn kind(self) -> DeclarationKind {
        self.0
    }

    /// Returns the plural heading one member group prints.
    #[must_use]
    pub const fn plural(kind: DeclarationKind) -> &'static str {
        match kind {
            DeclarationKind::Module => "modules",
            DeclarationKind::Class => "classes",
            DeclarationKind::Function => "functions",
            DeclarationKind::Method => "methods",
            DeclarationKind::Interface => "interfaces",
            DeclarationKind::Type => "types",
            DeclarationKind::Macro => "macros",
            DeclarationKind::Constant => "constants",
            DeclarationKind::Field => "fields",
            DeclarationKind::Property => "properties",
            DeclarationKind::Constructor => "constructors",
            DeclarationKind::Enum => "enums",
            DeclarationKind::Struct => "structs",
            DeclarationKind::Trait => "traits",
            DeclarationKind::Union => "unions",
            DeclarationKind::Variable => "variables",
            DeclarationKind::Import => "imports",
            DeclarationKind::Variant => "variants",
            DeclarationKind::Unknown => "declarations",
        }
    }
}

impl fmt::Display for KindGlyph {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The display glyph for one source language.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LanguageGlyph(Language);

impl LanguageGlyph {
    /// Selects the glyph for one language.
    #[must_use]
    pub const fn new(language: Language) -> Self {
        Self(language)
    }

    /// Returns the short display tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self.0 {
            Language::Rust => "rs",
            Language::TypeScript => "ts",
            Language::Python => "py",
            Language::Go => "go",
            Language::Java => "jv",
            Language::CSharp => "cs",
            Language::C => "c",
            Language::Cxx => "c+",
            Language::Unknown => "··",
        }
    }

    /// Returns the language this glyph stands for.
    #[must_use]
    pub const fn language(self) -> Language {
        self.0
    }
}

impl fmt::Display for LanguageGlyph {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Which way a relation points away from the declaration being read.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RelationDirection {
    /// The read declaration is the source of the relation.
    Outgoing,
    /// The read declaration is the target of the relation.
    Incoming,
}

/// The readable label of one relation group.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RelationLabel {
    /// A relation whose compiler kind and direction are both known.
    Typed(SemanticLinkKind, RelationDirection),
    /// A bounded neighbourhood whose per-edge kind the reply did not carry.
    Neighbourhood,
    /// Incoming and outgoing neighbours whose per-edge kind is not carried.
    Related,
}

impl RelationLabel {
    /// Returns the words this group prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Typed(kind, direction) => typed_label(kind, direction),
            Self::Neighbourhood => "neighbours",
            Self::Related => "related",
        }
    }
}

impl fmt::Display for RelationLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Returns the readable label for one compiler relation kind and direction.
///
/// The incoming spelling is the passive voice of the outgoing one, so a reader
/// never has to work out which end of the edge they are standing on.
#[must_use]
pub const fn relation_label(
    kind: SemanticLinkKind,
    direction: RelationDirection,
) -> RelationLabel {
    RelationLabel::Typed(kind, direction)
}

const fn typed_label(kind: SemanticLinkKind, direction: RelationDirection) -> &'static str {
    match direction {
        RelationDirection::Outgoing => match kind {
            SemanticLinkKind::Calls => "calls",
            SemanticLinkKind::MethodCall => "calls method",
            SemanticLinkKind::TypeReference => "uses type",
            SemanticLinkKind::Reads => "reads",
            SemanticLinkKind::Writes => "writes",
            SemanticLinkKind::Imports => "imports",
            SemanticLinkKind::Implements => "implements",
            SemanticLinkKind::Overrides => "overrides",
            SemanticLinkKind::Reexports => "re-exports",
            SemanticLinkKind::Inherits => "inherits",
            SemanticLinkKind::Documents => "documents",
        },
        RelationDirection::Incoming => match kind {
            SemanticLinkKind::Calls => "called by",
            SemanticLinkKind::MethodCall => "method called by",
            SemanticLinkKind::TypeReference => "used as type by",
            SemanticLinkKind::Reads => "read by",
            SemanticLinkKind::Writes => "written by",
            SemanticLinkKind::Imports => "imported by",
            SemanticLinkKind::Implements => "implemented by",
            SemanticLinkKind::Overrides => "overridden by",
            SemanticLinkKind::Reexports => "re-exported by",
            SemanticLinkKind::Inherits => "inherited by",
            SemanticLinkKind::Documents => "documented by",
        },
    }
}
