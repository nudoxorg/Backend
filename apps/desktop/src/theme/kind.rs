//! Declaration kinds rendered as a lettered glyph on one chromatic plane.
//!
//! A member list is scanned, not read. Giving every kind a distinct hue at an
//! identical lightness makes the list preattentively sortable — the eye finds
//! "all the traits" before it reads a single name — while the shared plane
//! means the same list degrades to one even texture in greyscale rather than
//! to noise. The letter carries the meaning when colour cannot.

use super::ramp::Hue;
use backend_library::DeclarationKind;
use backend_present::KindGlyph as SharedGlyph;

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

/// Returns the plural group header for one kind.
///
/// The words come from [`backend_present::KindGlyph::plural`], so a member
/// group in this window and a member group in `backend page` are headed by the
/// same noun; only the capitalisation is this surface's own.
pub(crate) fn group_title(kind: DeclarationKind) -> String {
    let plural = SharedGlyph::plural(kind);
    let mut title = String::with_capacity(plural.len());
    for (at, letter) in plural.chars().enumerate() {
        if at == 0 {
            title.extend(letter.to_uppercase());
        } else {
            title.push(letter);
        }
    }
    title
}

/// Every declaration kind, in the order a member list groups them.
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
