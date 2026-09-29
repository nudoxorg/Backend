//! FACET v3 tokens: colour, type, geometry and motion.
//!
//! Every value is transcribed from the live (v3) cascade of the design boards
//! (`Nudox-Design-System/spec/design-spec.md`). Views never invent a colour,
//! a size or a duration: they name a token. Colours are stored as
//! [`Tone`] constants so the palette is plain compile-time data; convert with
//! `.into()` wherever GPUI wants an `Hsla`, an `Rgba` or a `Background`.

// Colour literals are written exactly as the boards' CSS spells them.
#![allow(clippy::unreadable_literal)]

use gpui::{Background, Hsla, Pixels, Rgba, px};

/// A colour token: `0xRRGGBB` plus alpha. Plain data, `const`-constructible.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    rgb: u32,
    alpha: f32,
}

impl Tone {
    /// The same colour at `alpha` times its current opacity.
    #[must_use]
    pub const fn alpha(self, alpha: f32) -> Self {
        Self {
            rgb: self.rgb,
            alpha: self.alpha * alpha,
        }
    }

    /// The colour's opacity.
    #[must_use]
    pub const fn opacity(self) -> f32 {
        self.alpha
    }

    /// The colour as GPUI's `Rgba`.
    #[must_use]
    pub fn rgba(self) -> Rgba {
        let [_, r, g, b] = self.rgb.to_be_bytes();
        Rgba::new(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            self.alpha,
        )
    }

    /// The colour as GPUI's `Hsla`.
    #[must_use]
    pub fn hsla(self) -> Hsla {
        gpui::rgb_to_hsla(self.rgba())
    }

    /// Linear blend towards `other` by `t` in sRGB (for animated recolouring).
    #[must_use]
    pub fn mix(self, other: Self, t: f32) -> Hsla {
        let a = self.rgba();
        let b = other.rgba();
        let lerp = |x: f32, y: f32| x + (y - x) * t;
        gpui::rgb_to_hsla(Rgba::new(
            lerp(a.red, b.red),
            lerp(a.green, b.green),
            lerp(a.blue, b.blue),
            lerp(a.alpha, b.alpha),
        ))
    }
}

impl From<Tone> for Hsla {
    fn from(tone: Tone) -> Self {
        tone.hsla()
    }
}

impl From<Tone> for Rgba {
    fn from(tone: Tone) -> Self {
        tone.rgba()
    }
}

impl From<Tone> for Background {
    fn from(tone: Tone) -> Self {
        tone.hsla().into()
    }
}

/// Builds an opaque colour from `0xRRGGBB` at compile time.
#[must_use]
pub const fn hex(value: u32) -> Tone {
    hexa(value, 1.0)
}

/// Builds a colour from `0xRRGGBB` and an alpha in `0.0..=1.0`.
#[must_use]
pub const fn hexa(value: u32, alpha: f32) -> Tone {
    Tone { rgb: value, alpha }
}

/// The two appearances. Abyss is the dark default; Glacier is daylight.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Appearance {
    /// The dark blue abyss board.
    #[default]
    Abyss,
    /// The daylight glacier board.
    Glacier,
}

impl Appearance {
    /// The palette for this appearance.
    #[must_use]
    pub const fn palette(self) -> &'static Palette {
        match self {
            Self::Abyss => &ABYSS,
            Self::Glacier => &GLACIER,
        }
    }

    /// Whether this is the dark appearance.
    #[must_use]
    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Abyss)
    }
}

/// The five symbol families: hue = family, shape = kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Family {
    /// Modules, packages, imports.
    Namespace,
    /// Structs, classes, enums, unions, type aliases.
    Type,
    /// Traits and interfaces.
    Contract,
    /// Functions, methods, constructors, macros.
    Callable,
    /// Constants, fields, properties, variables, variants (the one warm hue).
    Value,
}

/// A voice: the four state colours the bevel speaks with.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Voice {
    /// Mint acts; yours; done.
    Mint,
    /// Periwinkle focuses.
    Peri,
    /// Amber waits.
    Amber,
    /// Coral stops.
    Coral,
}

/// One voice's three strengths: the colour, a soft fill, and a line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceTone {
    /// Full strength (text, bevel light side).
    pub base: Tone,
    /// Soft tint for fills.
    pub soft: Tone,
    /// Line strength for outlines and the bevel's shaded side.
    pub line: Tone,
}

/// One family's hue and its background tint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FamilyTone {
    /// The family hue.
    pub hue: Tone,
    /// A soft tint of the hue.
    pub bg: Tone,
}

/// Syntax colours for code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Syntax {
    /// Keywords.
    pub keyword: Tone,
    /// Type names.
    pub type_name: Tone,
    /// Function names.
    pub function: Tone,
    /// String literals.
    pub string: Tone,
    /// Numeric literals.
    pub number: Tone,
    /// Comments (rendered italic).
    pub comment: Tone,
    /// Punctuation.
    pub punctuation: Tone,
    /// Lifetimes and macros.
    pub macro_name: Tone,
    /// Attributes.
    pub attribute: Tone,
    /// Parameters.
    pub parameter: Tone,
    /// Constants.
    pub constant: Tone,
    /// Traits and interfaces.
    pub contract: Tone,
}

/// The complete colour vocabulary of one appearance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Behind the window.
    pub g0: Tone,
    /// Window ground.
    pub g1: Tone,
    /// Ring track base.
    pub g2: Tone,
    /// Graph node fill.
    pub g3: Tone,
    /// Tooltip and menu chrome fallback.
    pub g4: Tone,
    /// Tracks (slider, switch).
    pub g5: Tone,
    /// Hairline dividers.
    pub line1: Tone,
    /// Control outlines.
    pub line2: Tone,
    /// Hover and active outlines.
    pub line3: Tone,
    /// Every cut plate's flat fill.
    pub plate: Tone,
    /// Controls, nodules, the header tone of a two-tone plate.
    pub plate2: Tone,
    /// Hover and pressed plates, tooltip and menu bodies.
    pub plate3: Tone,
    /// The recessed table level (gem centre, deep plates).
    pub table: Tone,
    /// Names; yours; highest ownership.
    pub ink0: Tone,
    /// Body text and rows.
    pub ink1: Tone,
    /// Secondary text.
    pub ink2: Tone,
    /// Not yours; captions.
    pub ink3: Tone,
    /// Rules; closed doors.
    pub ink4: Tone,
    /// Bevel light side.
    pub bevel_hi: Tone,
    /// Bevel shaded side.
    pub bevel_lo: Tone,
    /// Mint voice.
    pub mint: VoiceTone,
    /// Periwinkle voice.
    pub peri: VoiceTone,
    /// Amber voice.
    pub amber: VoiceTone,
    /// Coral voice.
    pub coral: VoiceTone,
    /// The brighter periwinkle used on the focus bevel's light side.
    pub peri_hi: Tone,
    /// Teal, the mint's cooler sibling.
    pub teal: Tone,
    /// Leaf, the mint's deeper sibling.
    pub leaf: Tone,
    /// Text drawn on a mint fill.
    pub mint_ink: Tone,
    /// Namespaces.
    pub f_ns: FamilyTone,
    /// Types.
    pub f_type: FamilyTone,
    /// Contracts.
    pub f_con: FamilyTone,
    /// Callables.
    pub f_call: FamilyTone,
    /// Values.
    pub f_val: FamilyTone,
    /// Floating glass (popover body).
    pub glass: Tone,
    /// Wells (recessed inputs).
    pub well: Tone,
    /// Panes.
    pub pane: Tone,
    /// Inset shading.
    pub inset: Tone,
    /// Faint tint for quiet fills.
    pub tint: Tone,
    /// Stronger tint.
    pub tint2: Tone,
    /// Modal scrim.
    pub veil: Tone,
    /// The faceted ground's polygon colour (drawn at 0.6–9 % opacity).
    pub ground: Tone,
    /// Drop shadow colour for floating plates.
    pub shadow: Tone,
    /// Code colours.
    pub syntax: Syntax,
}

impl Palette {
    /// The tone for a voice.
    #[must_use]
    pub const fn voice(&self, voice: Voice) -> VoiceTone {
        match voice {
            Voice::Mint => self.mint,
            Voice::Peri => self.peri,
            Voice::Amber => self.amber,
            Voice::Coral => self.coral,
        }
    }

    /// The tone for a family.
    #[must_use]
    pub const fn family(&self, family: Family) -> FamilyTone {
        match family {
            Family::Namespace => self.f_ns,
            Family::Type => self.f_type,
            Family::Contract => self.f_con,
            Family::Callable => self.f_call,
            Family::Value => self.f_val,
        }
    }
}

/// Abyss, the dark default.
pub static ABYSS: Palette = Palette {
    g0: hex(0x030814),
    g1: hex(0x060d1b),
    g2: hex(0x0a1424),
    g3: hex(0x0f1b2f),
    g4: hex(0x15243c),
    g5: hex(0x1c304f),
    line1: hexa(0x9eb0ff, 0.07),
    line2: hexa(0x9eb0ff, 0.12),
    line3: hexa(0x9eb0ff, 0.22),
    plate: hex(0x0b1526),
    plate2: hex(0x101d33),
    plate3: hex(0x16273f),
    table: hex(0x050b17),
    ink0: hex(0xf5f7fb),
    ink1: hex(0xd2d9e5),
    // Every reading ink clears 4.5:1 on every reading ground (see the tests);
    // ink4 is rules and inactive ticks only, never text.
    ink2: hex(0xa3aec1),
    ink3: hex(0x8591a8),
    ink4: hex(0x4c5870),
    bevel_hi: hexa(0xc4d2ff, 0.26),
    bevel_lo: hexa(0x000000, 0.6),
    mint: VoiceTone {
        base: hex(0x6cebad),
        soft: hexa(0x62e6a6, 0.12),
        line: hexa(0x62e6a6, 0.34),
    },
    peri: VoiceTone {
        base: hex(0x93a2fa),
        soft: hexa(0x93a2fa, 0.13),
        line: hexa(0x93a2fa, 0.4),
    },
    amber: VoiceTone {
        base: hex(0xf4bb6a),
        soft: hexa(0xf4bb6a, 0.13),
        line: hexa(0xf4bb6a, 0.38),
    },
    coral: VoiceTone {
        base: hex(0xff7a8a),
        soft: hexa(0xff7a8a, 0.13),
        line: hexa(0xff7a8a, 0.4),
    },
    peri_hi: hex(0xbcc6ff),
    teal: hex(0x3fcdc6),
    leaf: hex(0x2fb96c),
    mint_ink: hex(0x03231a),
    f_ns: FamilyTone {
        hue: hex(0xa9b6cc),
        bg: hexa(0xa9b6cc, 0.12),
    },
    // Types are teal: apart from mint (yours) in hue and by 16 L*, so they
    // stay apart under deuteranopia, protanopia and tritanopia.
    f_type: FamilyTone {
        hue: hex(0x2bb8c9),
        bg: hexa(0x2bb8c9, 0.13),
    },
    f_con: FamilyTone {
        hue: hex(0xd59cf5),
        bg: hexa(0xd59cf5, 0.13),
    },
    f_call: FamilyTone {
        hue: hex(0x8fa6ff),
        bg: hexa(0x8fa6ff, 0.14),
    },
    // Values have no hue: amber means caution only. A value's printed value
    // is its information; its mark is neutral ink.
    f_val: FamilyTone {
        hue: hex(0xd2d9e5),
        bg: hexa(0xd2d9e5, 0.1),
    },
    glass: hex(0x132039),
    well: hexa(0x020711, 0.55),
    pane: hexa(0x040a16, 0.35),
    inset: hexa(0x000000, 0.28),
    tint: hexa(0xbecdff, 0.07),
    tint2: hexa(0xbecdff, 0.13),
    veil: hexa(0x030814, 0.62),
    ground: hex(0x9eb0ff),
    shadow: hexa(0x000000, 0.7),
    syntax: Syntax {
        keyword: hex(0xc5a3ff),
        type_name: hex(0x2bb8c9),
        function: hex(0x8fa6ff),
        string: hex(0xf0c987),
        number: hex(0xf39b8a),
        comment: hex(0x74819a),
        punctuation: hex(0x74819a),
        macro_name: hex(0xff9ec4),
        attribute: hex(0x9aa6ba),
        parameter: hex(0xd2d9e5),
        constant: hex(0xf3c06e),
        contract: hex(0xd59cf5),
    },
};

/// Glacier, the daylight appearance.
pub static GLACIER: Palette = Palette {
    g0: hex(0xdfe5ee),
    g1: hex(0xeef2f7),
    g2: hex(0xf6f8fb),
    g3: hex(0xffffff),
    g4: hex(0xeef2f8),
    g5: hex(0xe2e9f3),
    line1: hexa(0x1e326e, 0.08),
    line2: hexa(0x1e326e, 0.14),
    line3: hexa(0x1e326e, 0.26),
    plate: hex(0xffffff),
    plate2: hex(0xf3f6fb),
    plate3: hex(0xe9eef6),
    table: hex(0xeef2f8),
    ink0: hex(0x0a1222),
    ink1: hex(0x1d2940),
    ink2: hex(0x39445b),
    // The quiet reading ink still clears 5:1 on Glacier's darkest ground,
    // with ink2 a clear step above it.
    ink3: hex(0x535d75),
    ink4: hex(0xa3aec2),
    bevel_hi: hex(0xffffff),
    bevel_lo: hexa(0x1e326e, 0.22),
    // Voices are read as text too ("yours" names, failure words): each base
    // clears 4.5:1 on every Glacier ground.
    mint: VoiceTone {
        base: hex(0x0b734e),
        soft: hexa(0x0f9d6a, 0.1),
        line: hexa(0x0f9d6a, 0.36),
    },
    peri: VoiceTone {
        base: hex(0x4757d5),
        soft: hexa(0x4b5bd6, 0.1),
        line: hexa(0x4b5bd6, 0.4),
    },
    amber: VoiceTone {
        base: hex(0x8f5609),
        soft: hexa(0xa8650a, 0.1),
        line: hexa(0xa8650a, 0.36),
    },
    coral: VoiceTone {
        base: hex(0xb42c43),
        soft: hexa(0xc8324a, 0.09),
        line: hexa(0xc8324a, 0.36),
    },
    peri_hi: hex(0x3443b8),
    teal: hex(0x0d8f8f),
    leaf: hex(0x0c8a4e),
    mint_ink: hex(0xffffff),
    f_ns: FamilyTone {
        hue: hex(0x55647e),
        bg: hexa(0x55647e, 0.1),
    },
    f_type: FamilyTone {
        hue: hex(0x055062),
        bg: hexa(0x055062, 0.1),
    },
    f_con: FamilyTone {
        hue: hex(0x8b3fc0),
        bg: hexa(0x8b3fc0, 0.1),
    },
    f_call: FamilyTone {
        hue: hex(0x3d52d0),
        bg: hexa(0x3d52d0, 0.1),
    },
    f_val: FamilyTone {
        hue: hex(0x1d2940),
        bg: hexa(0x1d2940, 0.08),
    },
    glass: hex(0xffffff),
    well: hexa(0x1e326e, 0.05),
    pane: hexa(0xffffff, 0.6),
    inset: hexa(0x1e326e, 0.08),
    tint: hexa(0x1e326e, 0.05),
    tint2: hexa(0x1e326e, 0.09),
    veil: hexa(0xdfe5ee, 0.62),
    ground: hex(0x1e326e),
    shadow: hexa(0x14285a, 0.3),
    syntax: Syntax {
        keyword: hex(0x7b3fd1),
        type_name: hex(0x055062),
        function: hex(0x3d52d0),
        string: hex(0x9a5b00),
        number: hex(0xb8432f),
        comment: hex(0x66758f),
        punctuation: hex(0x66758f),
        macro_name: hex(0xb8327a),
        attribute: hex(0x4b5a75),
        parameter: hex(0x1d2940),
        constant: hex(0xa2650c),
        contract: hex(0x8b3fc0),
    },
};

/// The four faces, one job each.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Face {
    /// Bricolage Grotesque: names, heroes, section heads. Never below 16 px.
    Display,
    /// Geist: controls, rows, body.
    Ui,
    /// Geist Mono: every identifier, path and version.
    Mono,
    /// Newsreader, always italic: ledes, captions, margin notes.
    Serif,
}

impl Face {
    /// The family name `CoreText`, `DirectWrite` and fontconfig resolve.
    #[must_use]
    pub const fn family(self) -> &'static str {
        match self {
            Self::Display => "Bricolage Grotesque",
            Self::Ui => "Geist",
            Self::Mono => "Geist Mono",
            Self::Serif => "Newsreader",
        }
    }
}

/// One rung of the type ladder, sized at 100 % text scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypeRole {
    /// The face.
    pub face: Face,
    /// Weight on the 100–900 axis (variable faces take any value).
    pub weight: f32,
    /// Size in px at 100 % text scale.
    pub size: f32,
    /// Line height in px at 100 % text scale.
    pub line: f32,
    /// Tracking in em (negative tightens).
    pub tracking: f32,
    /// Italic (the serif is always italic).
    pub italic: bool,
}

const fn role(face: Face, weight: f32, size: f32, line: f32, tracking: f32) -> TypeRole {
    TypeRole {
        face,
        weight,
        size,
        line,
        tracking,
        italic: matches!(face, Face::Serif),
    }
}

/// The type ladder.
pub mod ty {
    use super::{Face, TypeRole, role};

    /// Board heroes ("Descent").
    pub const DISPLAY_XL: TypeRole = role(Face::Display, 640.0, 46.0, 50.0, -0.035);
    /// A symbol or package name at the head of its page.
    pub const HERO: TypeRole = role(Face::Display, 640.0, 44.0, 46.0, -0.035);
    /// Section heads and doc h2.
    pub const DISPLAY: TypeRole = role(Face::Display, 620.0, 30.0, 36.0, -0.025);
    /// Panel titles.
    pub const TITLE: TypeRole = role(Face::Display, 650.0, 20.0, 26.0, -0.01);
    /// The shelf's book title.
    pub const BOOK: TypeRole = role(Face::Display, 640.0, 17.0, 22.0, -0.02);
    /// The altimeter's current depth.
    pub const DEPTH: TypeRole = role(Face::Display, 620.0, 13.0, 16.0, -0.01);
    /// Row and list heads.
    pub const HEAD: TypeRole = role(Face::Ui, 600.0, 15.0, 22.0, 0.0);
    /// Dialog titles.
    pub const DIALOG: TypeRole = role(Face::Ui, 500.0, 15.0, 20.0, 0.0);
    /// Longer paragraphs.
    pub const PROSE: TypeRole = role(Face::Ui, 400.0, 14.5, 23.0, 0.0);
    /// Default body.
    pub const BODY: TypeRole = role(Face::Ui, 400.0, 13.5, 21.0, 0.0);
    /// Rows in dense lists.
    pub const ROW: TypeRole = role(Face::Ui, 400.0, 13.0, 18.0, 0.0);
    /// Buttons.
    pub const BUTTON: TypeRole = role(Face::Ui, 500.0, 12.0, 16.0, 0.0);
    /// Meta text.
    pub const SMALL: TypeRole = role(Face::Ui, 400.0, 12.0, 16.0, 0.0);
    /// Field labels and eyebrows.
    pub const LABEL: TypeRole = role(Face::Ui, 600.0, 10.5, 14.0, 0.1);
    /// Status bar text.
    pub const STATUS: TypeRole = role(Face::Ui, 400.0, 11.0, 14.0, 0.0);
    /// Code and identifiers in running text.
    pub const CODE: TypeRole = role(Face::Mono, 400.0, 12.5, 20.0, 0.0);
    /// Identifiers in rows and chips.
    pub const MONO_ROW: TypeRole = role(Face::Mono, 400.0, 12.5, 18.0, 0.0);
    /// Small identifiers (versions, counts, paths in margins).
    pub const MONO_SMALL: TypeRole = role(Face::Mono, 400.0, 11.5, 16.0, 0.0);
    /// Documentation ledes.
    pub const LEDE: TypeRole = role(Face::Serif, 400.0, 19.0, 28.0, 0.0);
    /// Margin notes.
    pub const MARGIN: TypeRole = role(Face::Serif, 400.0, 13.5, 19.0, 0.0);
    /// Captions and quiet hints.
    pub const CAPTION: TypeRole = role(Face::Serif, 400.0, 13.0, 18.0, 0.0);
    /// Rose axis labels.
    pub const AXIS: TypeRole = role(Face::Serif, 400.0, 13.0, 16.0, 0.0);
}

/// The one type scale (DIRECTION.md §5): six roles, no exceptions. Pages use
/// only these; the shell and chrome migrate from [`ty`] in a later sweep.
pub mod scale {
    use super::{Face, TypeRole, role};

    /// The page title: the only large text on a page.
    pub const DISPLAY: TypeRole = role(Face::Display, 700.0, 40.0, 44.0, -0.03);
    /// The author's first sentence, and at most one sentence per section.
    pub const LEDE: TypeRole = role(Face::Serif, 400.0, 19.0, 28.0, 0.0);
    /// A section heading, at ink2 beside its gutter mark.
    pub const SECTION: TypeRole = role(Face::Ui, 600.0, 13.0, 16.0, 0.02);
    /// Reading text.
    pub const BODY: TypeRole = role(Face::Ui, 400.0, 14.0, 22.0, 0.0);
    /// Every identifier, path and version.
    pub const MONO: TypeRole = role(Face::Mono, 400.0, 13.0, 20.0, 0.0);
    /// A name in a specimen: the mono role at weight 500.
    pub const MONO_NAME: TypeRole = role(Face::Mono, 500.0, 13.0, 20.0, 0.0);
    /// Labels, counts and quiet words, at ink3.
    pub const LABEL: TypeRole = role(Face::Ui, 400.0, 12.0, 16.0, 0.0);
    /// Counts, versions and paths at label size.
    pub const LABEL_MONO: TypeRole = role(Face::Mono, 400.0, 12.0, 16.0, 0.0);
}

/// The page's rhythm, in px at 100 % text scale, on an 8 px grid.
pub mod rhythm {
    /// The grid every measure sits on.
    pub const GRID: f32 = 8.0;
    /// Between sections.
    pub const SECTION: f32 = 48.0;
    /// Between groups inside a section.
    pub const GROUP: f32 = 16.0;
    /// A specimen row, a rail.
    pub const ROW: f32 = 32.0;
    /// A verb row, a policy row.
    pub const ROW_TIGHT: f32 = 28.0;
    /// A member row's pitch: one row per member, Mono 13/20 (the wave-6
    /// compactness law).
    pub const ROW_PITCH: f32 = 24.0;
    /// From a section's heading to its content.
    pub const HEAD_GAP: f32 = 20.0;
    /// The reading column.
    pub const COLUMN: f32 = 640.0;
    /// How far the spine sits left of the column.
    pub const SPINE: f32 = 44.0;
    /// The spine's offset below the shelf breakpoint.
    pub const SPINE_NARROW: f32 = 36.0;
    /// The hero gem.
    pub const GEM: f32 = 56.0;
    /// The hero gem below the shelf breakpoint.
    pub const GEM_NARROW: f32 = 40.0;
}

/// Stroke weights and dashes. Weight says what a line is; the dash says how
/// sure the page is of it.
pub mod stroke {
    /// Structure: rules, rungs, partitions.
    pub const HAIR: f32 = 1.0;
    /// A relation or a specimen's own shape, in the kind hue.
    pub const RELATION: f32 = 1.5;
    /// Focus: the bevel.
    pub const FOCUS: f32 = 2.0;
    /// Matched by name or path, not compiler-resolved.
    pub const INFERRED: [f32; 2] = [5.0, 4.0];
    /// Optional or maybe.
    pub const OPTIONAL: [f32; 2] = [1.5, 3.0];
    /// A branch that exists in the type but not on this path.
    pub const PRUNED: [f32; 2] = [2.0, 3.0];
}

/// Geometry: chamfers, radii and shell metrics, in px at 100 % text scale.
pub mod geo {
    use gpui::{Pixels, px};

    /// Small chamfer (rows, chips, compact plates).
    pub const CUT_SM: Pixels = px(9.0);
    /// Default chamfer.
    pub const CUT: Pixels = px(14.0);
    /// Large chamfer (dialogs, the ask plate).
    pub const CUT_LG: Pixels = px(22.0);
    /// Button chamfer.
    pub const CUT_BTN: Pixels = px(8.0);
    /// The here capsule and floating plates.
    pub const CUT_FLOAT: Pixels = px(10.0);
    /// Tooltips and comb popups.
    pub const CUT_TIP: Pixels = px(6.0);
    /// Mosaic stones.
    pub const CUT_STONE: Pixels = px(3.0);
    /// A rail's step plate.
    pub const CUT_STEP: Pixels = px(6.0);
    /// A specimen's own plate (the pipe's body, the contract's socket).
    pub const CUT_SPEC: Pixels = px(8.0);
    /// The package page's type case.
    pub const CUT_TRAY: Pixels = px(12.0);
    /// Bevel width at rest; focus doubles it.
    pub const BEVEL: Pixels = px(1.0);

    /// Radii (used sparingly; most surfaces are cut, not rounded).
    pub const R1: Pixels = px(5.0);
    /// Radius 2.
    pub const R2: Pixels = px(8.0);
    /// Radius 3.
    pub const R3: Pixels = px(12.0);
    /// Radius 4.
    pub const R4: Pixels = px(16.0);

    /// Titlebar height.
    pub const TITLEBAR: Pixels = px(50.0);
    /// Status bar height.
    pub const STATUS: Pixels = px(26.0);
    /// Shelf default width.
    pub const SHELF: Pixels = px(264.0);
    /// Shelf minimum width while resizing.
    pub const SHELF_MIN: Pixels = px(200.0);
    /// Shelf maximum width while resizing.
    pub const SHELF_MAX: Pixels = px(420.0);
    /// The collapsed shelf spine.
    pub const KSPINE: Pixels = px(42.0);
    /// The third column of pinned peeks.
    pub const PINS: Pixels = px(320.0);
    /// Reading column maximum.
    pub const FOLIO_MAX: Pixels = px(1080.0);
    /// Margin note column.
    pub const MARGIN: Pixels = px(250.0);
    /// Gutter between the folio and the margin.
    pub const MARGIN_GUTTER: Pixels = px(34.0);
    /// Splitter hit zone.
    pub const SPLIT_HIT: Pixels = px(9.0);
    /// Shelf row minimum height.
    pub const ROW: Pixels = px(27.0);
    /// Default control height.
    pub const CONTROL: Pixels = px(30.0);
    /// Small control height.
    pub const CONTROL_SM: Pixels = px(24.0);
    /// Large control height.
    pub const CONTROL_LG: Pixels = px(38.0);
    /// Input height.
    pub const INPUT: Pixels = px(32.0);
    /// The here capsule height.
    pub const HERE: Pixels = px(34.0);
}

/// Fluid tokens: every size that depends on the room a region has, as a ramp
/// between end points (`facet::fluid`), and every genuine change of
/// arrangement, as an ordered set of modes with hysteresis. This is the one
/// place a width becomes a layout decision: nothing else in the app compares
/// a width with a number.
///
/// Rooms are in design px (width at 100 % text); lengths read back in real px.
pub mod fluid {
    use crate::fluid::{Blend, Grid, Ladder, Length, ModeId, rung, stop};

    // ---- The shell's frame ----

    /// How the shelf sits in the window.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Dock {
        /// Not inline: a drawer over the page, opened from the titlebar.
        Drawer,
        /// The 42 px kspine inline; the full shelf opens over the page.
        Spine,
        /// The full shelf inline.
        Shelf,
    }

    /// Shelf beside the page from 900, a spine from 640, a drawer below.
    pub const DOCK: Ladder<Dock> = Ladder::new(
        ModeId::Dock,
        &[rung(Dock::Drawer, 0.0), rung(Dock::Spine, 640.0), rung(Dock::Shelf, 900.0)],
    );

    /// Whether pinned peeks have a column of their own.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Pins {
        /// Pins float over the page.
        Over,
        /// A third column beside the page.
        Column,
    }

    /// The page keeps its full measure beside a third column from 1900.
    pub const PINS: Ladder<Pins> = Ladder::new(ModeId::Pins, &[rung(Pins::Over, 0.0), rung(Pins::Column, 1900.0)]);

    /// The most of the window the shelf and the pins column may take while they
    /// move, as a share of it: on a fast shrink the columns are still on their
    /// way to their new width, and the reader must not be squeezed to a sliver
    /// meanwhile (the columns clip, they do not reflow). 42 % on a phone, 32 %
    /// from 900, where the shelf at rest (264) is 29 %.
    pub const COLUMNS_SHARE: Blend = Blend::new(&[stop(320.0, 0.42), stop(900.0, 0.32)]);

    /// The drawer's scrim strip: the part of the page left showing beside an
    /// open drawer, so a click there can close it.
    pub const DRAWER_STRIP: Length = Length::new(&[stop(320.0, 48.0), stop(640.0, 96.0)]);

    /// What the titlebar carries.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Bar {
        /// The toggle, the place's name and Search.
        Bare,
        /// Adds the inbox and the trail to the name, less the package.
        Snug,
        /// Everything: the view switch and the whole trail.
        Full,
    }

    /// The titlebar's controls arrive from 560 and 760.
    pub const BAR: Ladder<Bar> = Ladder::new(ModeId::Bar, &[rung(Bar::Bare, 0.0), rung(Bar::Snug, 560.0), rung(Bar::Full, 760.0)]);

    // ---- The reader ----

    /// The reader's side gutter: 16 px on a phone, the design's 22 at 480,
    /// 40 at 1600.
    pub const READER_PAD: Length = Length::new(&[stop(320.0, 16.0), stop(480.0, 22.0), stop(1600.0, 40.0)]).smooth();

    /// The space above a page.
    pub const READER_TOP: Length = Length::new(&[stop(320.0, 16.0), stop(480.0, 22.0), stop(1600.0, 56.0)]).smooth();

    /// The measure wide content (tables, rails, comparisons) may take:
    /// the reading column until 1440, then growing to fill a big window.
    /// Prose keeps the reading column.
    pub const WIDE_FOLIO: Length = Length::new(&[stop(1440.0, 784.0), stop(2560.0, 1120.0)]);

    /// Where margin notes sit.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Notes {
        /// Folded under their block.
        Under,
        /// Beside their block, in a margin.
        Beside,
    }

    /// Notes sit in a margin from 1100.
    pub const NOTES: Ladder<Notes> = Ladder::new(ModeId::Notes, &[rung(Notes::Under, 0.0), rung(Notes::Beside, 1100.0)]);

    // ---- Rhythm and type, everywhere ----

    /// How much gaps breathe: tight on a phone, roomy in a big window.
    pub const BREATHE: Blend = Blend::new(&[stop(320.0, 0.66), stop(480.0, 0.78), stop(1600.0, 1.12), stop(2560.0, 1.3)]).smooth();

    /// Display type's share of its size: a page title shrinks on a phone and
    /// grows in a big window; body text never does.
    pub const DISPLAY: Blend = Blend::new(&[stop(320.0, 0.70), stop(480.0, 0.78), stop(1600.0, 1.0), stop(2560.0, 1.2)]).smooth();

    /// What the gallery titlebar draws: the flow targets' container queries.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Beads {
        /// The thread fills the bar; no buttons.
        Bare,
        /// The thread fills the bar; the shelf toggle and the buttons.
        Buttons,
        /// Two beads behind "here" and the forward half.
        Some,
        /// Three beads behind "here".
        All,
    }

    /// Buttons from 520, beads from 760, the oldest bead from 1100.
    pub const BEADS: Ladder<Beads> = Ladder::new(
        ModeId::Beads,
        &[rung(Beads::Bare, 0.0), rung(Beads::Buttons, 520.0), rung(Beads::Some, 760.0), rung(Beads::All, 1100.0)],
    );

    /// The mock window's shelf in the chrome gallery: 18 % of the window,
    /// between 220 and 264 (the flow targets).
    pub const MOCK_SHELF: Length = Length::new(&[stop(1222.22, 220.0), stop(1466.67, 264.0)]);

    /// The marks gallery's hero gem: 48 on a phone, 64 from 640.
    pub const MARK_GEM: Length = Length::new(&[stop(320.0, 48.0), stop(640.0, 64.0)]);

    // ---- The sidebar ----

    /// How the sidebar sets its lens strip and its rows for the room it has.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum SideForm {
        /// A lens you are not on shows its chord letter instead of its
        /// word, and a row drops the quiet words after its name.
        Tight,
        /// Every lens says its word, every row its quiet words.
        Full,
    }

    /// The sidebar is Full from 232 design px (its floor is 200, its
    /// default 264).
    pub const SIDE: Ladder<SideForm> = Ladder::new(ModeId::Side, &[rung(SideForm::Tight, 0.0), rung(SideForm::Full, 232.0)]);

    /// The marks gallery's dependency line sits beside the marks from 760.
    pub const HERO_DEPS: Ladder<Split> = Ladder::new(ModeId::Lab, &[rung(Split::Stacked, 0.0), rung(Split::Beside, 760.0)]);

    /// Where the graph's focus card sits.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Card {
        /// A sheet under the map, a gutter from its edges.
        Below,
        /// A card beside the map, at its right.
        Beside,
    }

    /// The focus card sits beside the map from 640.
    pub const CARD: Ladder<Card> = Ladder::new(ModeId::Card, &[rung(Card::Below, 0.0), rung(Card::Beside, 640.0)]);

    /// What the focus card takes from the map beside it, for the camera: its
    /// 340 px and the gutters around it.
    pub const CARD_ROOM: Length = Length::new(&[stop(640.0, 380.0), stop(1440.0, 380.0)]);

    /// How the graph sets a symbol's relations around it.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Reading {
        /// One merged column, to the right of the focus.
        Merged,
        /// Two columns, one each side of the focus.
        Columns,
    }

    /// The relations sit in two columns when the free room is 900 wide.
    pub const READING: Ladder<Reading> =
        Ladder::new(ModeId::Reading, &[rung(Reading::Merged, 0.0), rung(Reading::Columns, 900.0)]);

    /// How far a chain is framed back from its ends: a step further on a
    /// phone, where the map is small.
    pub const CHAIN_MARGIN: Blend = Blend::new(&[stop(560.0, 2.8), stop(720.0, 1.9)]);

    // ---- Ready for the page lanes (`.local/lanes/wave6/fluid/ADOPT.md`) ----
    //
    // The numbers the symbol and package pages compare a width with today,
    // as tokens and modes, so adopting them is a swap of names.

    /// Where the symbol page's rail sits.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Rail {
        /// Under the page.
        Below,
        /// Beside the page, in a column of its own.
        Beside,
    }

    /// The rail sits beside the page from 1100, held 40 px through the edge
    /// (`anatomy/symbol/layout.rs` `ENTER` 1120 / `LEAVE` 1080).
    pub const SYMBOL_RAIL: Ladder<Rail> =
        Ladder::new(ModeId::SymbolRail, &[rung(Rail::Below, 0.0), rung(Rail::Beside, 1100.0)]).banded(40.0);

    /// How a page sets a case's or a field's name, type and doc.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Rows {
        /// Stacked: a room too narrow for three columns.
        Stacked,
        /// Name, type and doc in columns.
        Columns,
    }

    /// Rows are in columns from 760 (`anatomy/symbol/body.rs` `stacked`).
    pub const SYMBOL_ROWS: Ladder<Rows> =
        Ladder::new(ModeId::SymbolRows, &[rung(Rows::Stacked, 0.0), rung(Rows::Columns, 760.0)]);

    /// How many columns of cells a page has.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Cells {
        /// One column.
        One,
        /// Two columns.
        Two,
    }

    /// Cells are in two columns from 900 (`anatomy/symbol/body.rs` `two`).
    pub const SYMBOL_CELLS: Ladder<Cells> =
        Ladder::new(ModeId::SymbolCells, &[rung(Cells::One, 0.0), rung(Cells::Two, 900.0)]);

    /// Whether the page is set for a phone.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Screen {
        /// A phone-width column: lists, not diagrams.
        Phone,
        /// A window.
        Window,
    }

    /// A phone below 480 (`anatomy/symbol/layout.rs` `phone`).
    pub const SYMBOL_PHONE: Ladder<Screen> =
        Ladder::new(ModeId::SymbolPhone, &[rung(Screen::Phone, 0.0), rung(Screen::Window, 480.0)]);

    /// The relations prism is one column on a rail below 620, columns above
    /// (`anatomy/prism.rs` `ONE_COLUMN_BELOW`).
    pub const SYMBOL_PRISM: Ladder<Cells> =
        Ladder::new(ModeId::SymbolPrism, &[rung(Cells::One, 0.0), rung(Cells::Two, 620.0)]);

    /// How far the page's spine sits left of its column (`rhythm::SPINE_NARROW`
    /// 36 below 720, `rhythm::SPINE` 44 above, as a glide).
    pub const PAGE_SPINE: Length = Length::new(&[stop(560.0, 36.0), stop(760.0, 44.0)]);

    /// The hero gem (`rhythm::GEM_NARROW` 40 below 720, `rhythm::GEM` 56 above).
    pub const PAGE_GEM: Length = Length::new(&[stop(560.0, 40.0), stop(760.0, 56.0)]);

    /// The gap between the symbol page's sections: 24 at 480, 34 from 1600.
    pub const SYMBOL_SECTION: Length = Length::new(&[stop(480.0, 24.0), stop(1600.0, 34.0)]).smooth();

    /// The width of the symbol page's label column (`GIVES`, a port's name).
    pub const SYMBOL_LABEL: Length = Length::new(&[stop(480.0, 58.0), stop(1600.0, 84.0)]).smooth();

    /// The width of a place's file column in "In your workspace".
    pub const SYMBOL_PLACE: Length = Length::new(&[stop(480.0, 132.0), stop(1600.0, 210.0)]).smooth();

    /// How the rose is drawn.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Form {
        /// Four quiet lines: `is  Display, ToString`.
        List,
        /// The field of four directions.
        Field,
    }

    /// The rose is four quiet lines below 560 and a field above; `Rose` reads
    /// it through a `Modes` of its own (`data/rose.rs`), and `Rose::list(..)`
    /// still forces one.
    pub const ROSE: Ladder<Form> = Ladder::new(ModeId::Rose, &[rung(Form::List, 0.0), rung(Form::Field, 560.0)]);

    /// How a version comb is drawn.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Comb {
        /// A band of ticks: all a column this narrow can hold.
        Band,
        /// The style the comb was asked for.
        Asked,
    }

    /// A comb is a band below 240 and the style it was asked for above
    /// (`marks/version.rs`), held 32 px through the edge.
    pub const COMB: Ladder<Comb> = Ladder::new(ModeId::Comb, &[rung(Comb::Band, 0.0), rung(Comb::Asked, 240.0)]);

    /// How many cells of the package page's crest share a row.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Crest {
        /// One under the other, on a phone.
        One,
        /// Two by two.
        Two,
        /// Four in a row: the licence a little wider than the rest.
        Four,
    }

    /// Two by two from 420, four in a row from 980, held 48 px through each
    /// edge (`bodies/package/fluid.rs` `TWO_FROM`, `FOUR_FROM`, `STICKY` 24).
    pub const CREST: Ladder<Crest> =
        Ladder::new(ModeId::Crest, &[rung(Crest::One, 0.0), rung(Crest::Two, 420.0), rung(Crest::Four, 980.0)]).banded(48.0);

    /// The package hero's gem (`bodies/package.rs` `fluid(48.0, 64.0)`).
    pub const PACKAGE_GEM: Length = Length::new(&[stop(480.0, 48.0), stop(1600.0, 64.0)]).smooth();

    /// The package page's symbol cards: as many columns as fit, each at least
    /// 262 px at 100 % text (240 on a phone, so one column fills a 320 window).
    pub const FOLIO_CARDS: Grid = Grid::new(ModeId::Folio, Length::new(&[stop(320.0, 240.0), stop(480.0, 262.0)]), 6);

    /// How Ask's results sit over the page.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Float {
        /// A sheet across the window, below the titlebar.
        Sheet,
        /// A panel over the shelf's column, at the left.
        Panel,
    }

    /// Ask is a floating panel from 640 and a sheet across the window below.
    pub const ASK: Ladder<Float> = Ladder::new(ModeId::Ask, &[rung(Float::Sheet, 0.0), rung(Float::Panel, 640.0)]);

    /// The results plate over the shelf's column: a panel from 320 to 440 px
    /// as the window grows (a sheet across the window below 640).
    pub const ASK_PANEL: Length = Length::new(&[stop(640.0, 320.0), stop(1440.0, 440.0)]);

    /// How a page sets its detail against its list.
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub enum Split {
        /// The detail under the row it belongs to.
        Stacked,
        /// The detail beside the list, in a column of its own.
        Beside,
    }

    /// Find's inspector sits beside its results from 760.
    pub const FIND: Ladder<Split> = Ladder::new(ModeId::Find, &[rung(Split::Stacked, 0.0), rung(Split::Beside, 760.0)]);

    /// Compare's row detail sits beside its rows from 760.
    pub const COMPARE: Ladder<Split> = Ladder::new(ModeId::Compare, &[rung(Split::Stacked, 0.0), rung(Split::Beside, 760.0)]);

    /// Find's inspector column: 38 % of the room, between 280 and 390.
    pub const FIND_INSPECTOR: Length = Length::new(&[stop(736.84, 280.0), stop(1026.32, 390.0)]);

    /// Compare's detail column: 52 % of the room, between 350 and 520.
    pub const COMPARE_DETAIL: Length = Length::new(&[stop(673.08, 350.0), stop(1000.0, 520.0)]);

    /// Compare's release columns sit side by side from 850.
    pub const COMPARE_COLUMNS: Ladder<Split> =
        Ladder::new(ModeId::CompareColumns, &[rung(Split::Stacked, 0.0), rung(Split::Beside, 850.0)]);

    /// The motion lab's cards: as many columns of 220 as fit, up to four.
    pub const LAB_CARDS: Grid = Grid::new(ModeId::Lab, Length::new(&[stop(320.0, 220.0), stop(1440.0, 220.0)]), 4);

    /// The gem of a project in the Library, read at the wide measure: 44 in the
    /// reading column, growing as the wide measure does.
    pub const PROJECT_GEM: Length = Length::new(&[stop(320.0, 34.0), stop(784.0, 44.0), stop(1120.0, 60.0)]);

    /// The gem of an empty Library.
    pub const EMPTY_GEM: Length = Length::new(&[stop(320.0, 40.0), stop(784.0, 56.0), stop(1120.0, 76.0)]);

    /// The Library's roles: two columns from 714 (two of 340 and the gap).
    pub const ROLES: Grid = Grid::new(ModeId::Library, Length::new(&[stop(320.0, 340.0), stop(1440.0, 340.0)]), 2);
}

/// Motion durations and curves.
pub mod motion {
    use std::time::Duration;

    /// Hover colour changes, press.
    pub const MICRO: Duration = Duration::from_millis(90);
    /// Tooltips, small reveals.
    pub const QUICK: Duration = Duration::from_millis(160);
    /// Plate lifts, most transitions.
    pub const STD: Duration = Duration::from_millis(240);
    /// Emphasis: the facet sweep, dialogs.
    pub const EMPH: Duration = Duration::from_millis(380);
    /// Scene changes: descent between depths.
    pub const SCENE: Duration = Duration::from_millis(620);

    /// A cubic-bezier easing curve, CSS convention.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Bezier {
        /// First control point x.
        pub x1: f32,
        /// First control point y.
        pub y1: f32,
        /// Second control point x.
        pub x2: f32,
        /// Second control point y.
        pub y2: f32,
    }

    /// Decelerate: things arriving and settling.
    pub const GLIDE: Bezier = Bezier { x1: 0.22, y1: 1.0, x2: 0.36, y2: 1.0 };
    /// A crisp snap.
    pub const SNAP: Bezier = Bezier { x1: 0.3, y1: 0.0, x2: 0.0, y2: 1.0 };
    /// A light overshoot.
    pub const SPRING: Bezier = Bezier { x1: 0.2, y1: 0.9, x2: 0.25, y2: 1.18 };
    /// The v3 default for plates and buttons: a stronger overshoot.
    pub const BOUNCE: Bezier = Bezier { x1: 0.34, y1: 1.56, x2: 0.64, y2: 1.0 };
    /// Accelerate: things leaving.
    pub const DROP: Bezier = Bezier { x1: 0.5, y1: 0.0, x2: 0.9, y2: 0.6 };
}

/// Converts a token size to pixels at a text scale (1.0 = 100 %).
#[must_use]
pub fn scaled(value: f32, scale: f32) -> Pixels {
    px(value * scale)
}

#[cfg(test)]
mod tests {
    use super::{ABYSS, GLACIER, Palette, Tone};

    fn linear(channel: f32) -> f32 {
        if channel <= 0.04045 { channel / 12.92 } else { ((channel + 0.055) / 1.055).powf(2.4) }
    }

    fn luminance(tone: Tone) -> f32 {
        let rgba = tone.rgba();
        0.2126 * linear(rgba.red) + 0.7152 * linear(rgba.green) + 0.0722 * linear(rgba.blue)
    }

    fn contrast(a: Tone, b: Tone) -> f32 {
        let (light, dark) = (luminance(a), luminance(b));
        (light.max(dark) + 0.05) / (light.min(dark) + 0.05)
    }

    /// The grounds reading text sits on (tracks, g5, carry no text).
    fn grounds(p: &Palette) -> [(&'static str, Tone); 10] {
        [("g0", p.g0), ("g1", p.g1), ("g2", p.g2), ("g3", p.g3), ("g4", p.g4), ("plate", p.plate), ("plate2", p.plate2), ("plate3", p.plate3), ("table", p.table), ("glass", p.glass)]
    }

    /// Every colour a page draws text in.
    fn text_roles(p: &Palette) -> [(&'static str, Tone); 8] {
        [("ink0", p.ink0), ("ink1", p.ink1), ("ink2", p.ink2), ("ink3", p.ink3), ("mint", p.mint.base), ("coral", p.coral.base), ("peri", p.peri.base), ("amber", p.amber.base)]
    }

    /// Every colour a page draws a meaningful mark or stroke in.
    fn mark_roles(p: &Palette) -> [(&'static str, Tone); 7] {
        [("type", p.f_type.hue), ("callable", p.f_call.hue), ("contract", p.f_con.hue), ("value", p.f_val.hue), ("namespace", p.f_ns.hue), ("mint", p.mint.base), ("coral", p.coral.base)]
    }

    /// Each failing pair, spelled out, so a regression names what broke.
    fn failures(p: &Palette, theme: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (ground, g) in grounds(p) {
            for (role, tone) in text_roles(p) {
                let c = contrast(tone, g);
                if c < 4.5 { out.push(format!("{theme}: text {role} on {ground} is {c:.2}:1, below 4.5")); }
            }
            for (role, tone) in mark_roles(p) {
                let c = contrast(tone, g);
                if c < 3.0 { out.push(format!("{theme}: mark {role} on {ground} is {c:.2}:1, below 3.0")); }
            }
        }
        out
    }

    #[test]
    fn every_reading_text_and_mark_clears_its_ground_in_both_themes() {
        let mut broken = failures(&ABYSS, "Abyss");
        broken.extend(failures(&GLACIER, "Glacier"));
        assert!(broken.is_empty(), "{}", broken.join("\n"));
    }

    #[test]
    fn the_inks_step_down_in_order_and_ink4_is_never_text() {
        for (theme, p) in [("Abyss", &ABYSS), ("Glacier", &GLACIER)] {
            let steps = [p.ink0, p.ink1, p.ink2, p.ink3].map(|ink| contrast(ink, p.g1));
            for pair in steps.windows(2) {
                assert!(pair[0] - pair[1] >= 1.2, "{theme}: ink steps {steps:?} collapse on g1");
            }
            // ink4 draws rules and inactive ticks; reading it would fail.
            assert!(contrast(p.ink4, p.g1) < 4.5, "{theme}: ink4 reads as text; rules must not");
            assert!(text_roles(p).iter().all(|(_, tone)| *tone != p.ink4), "{theme}: a text role is ink4");
        }
    }

    #[test]
    fn glacier_quiet_reading_ink_clears_its_darkest_ground() {
        for ground in [GLACIER.g0, GLACIER.g1, GLACIER.g2, GLACIER.g3] {
            assert!(contrast(GLACIER.ink3, ground) >= 5.1);
        }
        assert!(luminance(GLACIER.ink2) < luminance(GLACIER.ink3));
    }

    // ---- colour vision: Machado, Oliveira and Fernandes (2009), severity 1.0,
    // applied in linear sRGB; distance is CIE76 in L*a*b* (D65).
    const NORMAL: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    const DEUTAN: [[f32; 3]; 3] = [[0.367_322, 0.860_646, -0.227_968], [0.280_085, 0.672_501, 0.047_413], [-0.011_820, 0.042_940, 0.968_881]];
    const PROTAN: [[f32; 3]; 3] = [[0.152_286, 1.052_583, -0.204_868], [0.114_503, 0.786_281, 0.099_216], [-0.003_882, -0.048_116, 1.051_998]];
    const TRITAN: [[f32; 3]; 3] = [[1.255_528, -0.076_749, -0.178_779], [-0.078_411, 0.930_809, 0.147_602], [0.004_733, 0.691_367, 0.303_900]];

    fn lab(tone: Tone, m: &[[f32; 3]; 3]) -> [f32; 3] {
        let c = tone.rgba();
        let rgb = [linear(c.red), linear(c.green), linear(c.blue)];
        let s = m.map(|row| (row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]).clamp(0.0, 1.0));
        let x = 0.4124 * s[0] + 0.3576 * s[1] + 0.1805 * s[2];
        let y = 0.2126 * s[0] + 0.7152 * s[1] + 0.0722 * s[2];
        let z = 0.0193 * s[0] + 0.1192 * s[1] + 0.9505 * s[2];
        let f = |t: f32| if t > 0.008_856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
        [116.0 * f(y) - 16.0, 500.0 * (f(x / 0.950_47) - f(y)), 200.0 * (f(y) - f(z / 1.088_83))]
    }

    fn distance(a: Tone, b: Tone, m: &[[f32; 3]; 3]) -> f32 {
        let (a, b) = (lab(a, m), lab(b, m));
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    #[test]
    fn types_mint_and_callables_stay_apart_for_every_colour_vision() {
        for (theme, p) in [("Abyss", &ABYSS), ("Glacier", &GLACIER)] {
            let (teal, mint, peri) = (p.f_type.hue, p.mint.base, p.f_call.hue);
            for (vision, m, floor) in [("normal", &NORMAL, 25.0), ("deuteranopia", &DEUTAN, 25.0), ("protanopia", &PROTAN, 25.0), ("tritanopia", &TRITAN, 10.0)] {
                let tm = distance(teal, mint, m);
                let tp = distance(teal, peri, m);
                assert!(tm >= floor, "{theme} {vision}: type and yours are {tm:.1} apart, below {floor}");
                assert!(tp >= 14.0, "{theme} {vision}: type and callable are {tp:.1} apart, below 14");
            }
            // Apart by lightness as well as hue: a hue loss never merges them.
            let (lt, lm) = (lab(teal, &NORMAL)[0], lab(mint, &NORMAL)[0]);
            assert!((lt - lm).abs() >= 10.0, "{theme}: type L* {lt:.0} and yours L* {lm:.0} differ by under 10");
        }
    }
}
