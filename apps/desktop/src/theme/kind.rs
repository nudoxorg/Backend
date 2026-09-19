//! Declaration kinds on one chromatic plane, each with a drawn mark.
//!
//! A member list is scanned, not read. Giving every kind a distinct hue at an
//! identical lightness makes the list preattentively sortable — the eye finds
//! "all the traits" before it reads a single name — while the shared plane
//! means the same list degrades to one even texture in greyscale rather than
//! to noise. The mark carries the meaning when colour cannot.
//!
//! Marks are drawn, not lettered. An earlier version put `C` in the tile for
//! a class and `c` beside a C function's language, and a reader could not
//! tell which letter meant what. A struct is now a record with rows, an enum a
//! column of choices, a trait a diamond promising a core; the shapes are
//! distinct at fourteen pixels in a way nineteen letters never were. The
//! assets live in `assets/kinds` and are named by [`icon::kind_path`].

use super::ramp::Hue;
use crate::ui::icon;
use backend_library::DeclarationKind;
use backend_present::KindGlyph as SharedGlyph;

/// A kind's colour and spelled name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct KindGlyph {
    hue: Hue,
    label: &'static str,
}

impl KindGlyph {
    /// Returns the hue this kind occupies on the chromatic plane.
    pub(crate) const fn hue(self) -> Hue {
        self.hue
    }

    /// Returns the spelled kind name used in tooltips and group headers.
    pub(crate) const fn label(self) -> &'static str {
        self.label
    }
}

/// Returns the glyph for one declaration kind.
pub(crate) fn kind_glyph(kind: DeclarationKind) -> KindGlyph {
    let (degrees, label) = parts(kind);
    KindGlyph {
        hue: Hue::degrees(degrees),
        label,
    }
}

/// Returns the glyph used for rows that carry no typed kind.
pub(crate) fn untyped_glyph() -> KindGlyph {
    KindGlyph {
        hue: Hue::degrees(232.0),
        label: "declaration",
    }
}

/// Returns the glyph used for a package or project row.
pub(crate) fn package_glyph() -> KindGlyph {
    KindGlyph {
        hue: Hue::degrees(42.0),
        label: "package",
    }
}

/// Returns the asset the mark for one kind is drawn from.
pub(crate) const fn mark_path(kind: Option<DeclarationKind>) -> &'static str {
    icon::kind_path(kind)
}

const fn parts(kind: DeclarationKind) -> (f32, &'static str) {
    match kind {
        DeclarationKind::Module => (236.0, "module"),
        DeclarationKind::Import => (218.0, "import"),
        DeclarationKind::Class => (284.0, "class"),
        DeclarationKind::Struct => (158.0, "struct"),
        DeclarationKind::Enum => (128.0, "enum"),
        DeclarationKind::Variant => (112.0, "variant"),
        DeclarationKind::Union => (98.0, "union"),
        DeclarationKind::Interface => (306.0, "interface"),
        DeclarationKind::Trait => (324.0, "trait"),
        DeclarationKind::Type => (180.0, "type"),
        DeclarationKind::Function => (206.0, "function"),
        DeclarationKind::Method => (194.0, "method"),
        DeclarationKind::Constructor => (264.0, "constructor"),
        DeclarationKind::Macro => (342.0, "macro"),
        DeclarationKind::Constant => (40.0, "constant"),
        DeclarationKind::Field => (22.0, "field"),
        DeclarationKind::Property => (8.0, "property"),
        DeclarationKind::Variable => (62.0, "variable"),
        DeclarationKind::Unknown => (232.0, "declaration"),
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
pub(crate) const ALL_KINDS: [DeclarationKind; 19] = [
    DeclarationKind::Module,
    DeclarationKind::Struct,
    DeclarationKind::Class,
    DeclarationKind::Enum,
    DeclarationKind::Variant,
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
