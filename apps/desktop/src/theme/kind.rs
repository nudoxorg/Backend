//! Declaration kinds rendered as a lettered glyph on one chromatic plane.
//!
//! A member list is scanned, not read. Giving every kind a distinct hue at an
//! identical lightness makes the list preattentively sortable — the eye finds
//! "all the traits" before it reads a single name — while the shared plane
//! means the same list degrades to one even texture in greyscale rather than
//! to noise. The letter carries the meaning when colour cannot.

use super::ramp::Hue;
use backend_library::DeclarationKind;

/// A kind's mark: one letter and one hue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct KindGlyph {
    letter: char,
    hue: Hue,
    label: &'static str,
}

impl KindGlyph {
    /// Returns the letter drawn in the glyph tile.
    pub(crate) const fn letter(self) -> char {
        self.letter
    }

    /// Returns the hue this kind occupies on the chromatic plane.
    pub(crate) const fn hue(self) -> Hue {
        self.hue
    }

    /// Returns the spelled kind name used in tooltips and group headers.
    pub(crate) const fn label(self) -> &'static str {
        self.label
    }
}

/// Returns the mark for one declaration kind.
pub(crate) fn kind_glyph(kind: DeclarationKind) -> KindGlyph {
    let (letter, degrees, label) = parts(kind);
    KindGlyph {
        letter,
        hue: Hue::degrees(degrees),
        label,
    }
}

/// Returns the mark used for rows that carry no typed kind.
pub(crate) fn untyped_glyph() -> KindGlyph {
    KindGlyph {
        letter: '·',
        hue: Hue::degrees(232.0),
        label: "declaration",
    }
}

/// Returns the mark used for a package or project row.
pub(crate) fn package_glyph() -> KindGlyph {
    KindGlyph {
        letter: '◆',
        hue: Hue::degrees(42.0),
        label: "package",
    }
}

fn parts(kind: DeclarationKind) -> (char, f32, &'static str) {
    match kind {
        DeclarationKind::Module => ('M', 236.0, "module"),
        DeclarationKind::Import => ('↓', 218.0, "import"),
        DeclarationKind::Class => ('C', 284.0, "class"),
        DeclarationKind::Struct => ('S', 158.0, "struct"),
        DeclarationKind::Enum => ('E', 128.0, "enum"),
        DeclarationKind::Union => ('U', 104.0, "union"),
        DeclarationKind::Interface => ('I', 306.0, "interface"),
        DeclarationKind::Trait => ('R', 324.0, "trait"),
        DeclarationKind::Type => ('T', 180.0, "type"),
        DeclarationKind::Function => ('ƒ', 206.0, "function"),
        DeclarationKind::Method => ('m', 194.0, "method"),
        DeclarationKind::Constructor => ('+', 264.0, "constructor"),
        DeclarationKind::Macro => ('!', 342.0, "macro"),
        DeclarationKind::Constant => ('K', 40.0, "constant"),
        DeclarationKind::Field => ('f', 22.0, "field"),
        DeclarationKind::Property => ('p', 8.0, "property"),
        DeclarationKind::Variable => ('v', 62.0, "variable"),
        DeclarationKind::Unknown => ('·', 232.0, "declaration"),
    }
}

/// Returns the order kinds are grouped in on a declaration page.
///
/// Members are grouped structurally — what a thing *is*, then what it *does*,
/// then what it *holds* — which matches the order a reader asks those
/// questions in, rather than alphabetical order, which answers none of them.
pub(crate) fn group_rank(kind: DeclarationKind) -> u8 {
    match kind {
        DeclarationKind::Module => 0,
        DeclarationKind::Struct => 1,
        DeclarationKind::Class => 2,
        DeclarationKind::Enum => 3,
        DeclarationKind::Union => 4,
        DeclarationKind::Interface => 5,
        DeclarationKind::Trait => 6,
        DeclarationKind::Type => 7,
        DeclarationKind::Constructor => 8,
        DeclarationKind::Function => 9,
        DeclarationKind::Method => 10,
        DeclarationKind::Macro => 11,
        DeclarationKind::Property => 12,
        DeclarationKind::Field => 13,
        DeclarationKind::Constant => 14,
        DeclarationKind::Variable => 15,
        DeclarationKind::Import => 16,
        DeclarationKind::Unknown => 17,
    }
}

/// Returns the plural group header for one kind.
pub(crate) fn group_title(kind: DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Module => "Modules",
        DeclarationKind::Class => "Classes",
        DeclarationKind::Struct => "Structs",
        DeclarationKind::Enum => "Enums",
        DeclarationKind::Union => "Unions",
        DeclarationKind::Interface => "Interfaces",
        DeclarationKind::Trait => "Traits",
        DeclarationKind::Type => "Types",
        DeclarationKind::Function => "Functions",
        DeclarationKind::Method => "Methods",
        DeclarationKind::Constructor => "Constructors",
        DeclarationKind::Macro => "Macros",
        DeclarationKind::Constant => "Constants",
        DeclarationKind::Field => "Fields",
        DeclarationKind::Property => "Properties",
        DeclarationKind::Variable => "Variables",
        DeclarationKind::Import => "Imports",
        DeclarationKind::Unknown => "Declarations",
    }
}

/// Every kind, in group order, for the preview fixtures and the ramp tests.
pub(crate) const ALL_KINDS: [DeclarationKind; 18] = [
    DeclarationKind::Module,
    DeclarationKind::Struct,
    DeclarationKind::Class,
    DeclarationKind::Enum,
    DeclarationKind::Union,
    DeclarationKind::Interface,
    DeclarationKind::Trait,
    DeclarationKind::Type,
    DeclarationKind::Constructor,
    DeclarationKind::Function,
    DeclarationKind::Method,
    DeclarationKind::Macro,
    DeclarationKind::Property,
    DeclarationKind::Field,
    DeclarationKind::Constant,
    DeclarationKind::Variable,
    DeclarationKind::Import,
    DeclarationKind::Unknown,
];
