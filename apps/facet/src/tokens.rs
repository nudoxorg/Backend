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
    ink2: hex(0x9aa6ba),
    ink3: hex(0x74819a),
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
    f_type: FamilyTone {
        hue: hex(0x5fe0b4),
        bg: hexa(0x5fe0b4, 0.13),
    },
    f_con: FamilyTone {
        hue: hex(0xd59cf5),
        bg: hexa(0xd59cf5, 0.13),
    },
    f_call: FamilyTone {
        hue: hex(0x8fa6ff),
        bg: hexa(0x8fa6ff, 0.14),
    },
    f_val: FamilyTone {
        hue: hex(0xf3c06e),
        bg: hexa(0xf3c06e, 0.13),
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
        type_name: hex(0x5fe0b4),
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
    ink2: hex(0x4b5a75),
    ink3: hex(0x66758f),
    ink4: hex(0xa3aec2),
    bevel_hi: hex(0xffffff),
    bevel_lo: hexa(0x1e326e, 0.22),
    mint: VoiceTone {
        base: hex(0x0f9d6a),
        soft: hexa(0x0f9d6a, 0.1),
        line: hexa(0x0f9d6a, 0.36),
    },
    peri: VoiceTone {
        base: hex(0x4b5bd6),
        soft: hexa(0x4b5bd6, 0.1),
        line: hexa(0x4b5bd6, 0.4),
    },
    amber: VoiceTone {
        base: hex(0xa8650a),
        soft: hexa(0xa8650a, 0.1),
        line: hexa(0xa8650a, 0.36),
    },
    coral: VoiceTone {
        base: hex(0xc8324a),
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
        hue: hex(0x0b8f68),
        bg: hexa(0x0b8f68, 0.1),
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
        hue: hex(0xa2650c),
        bg: hexa(0xa2650c, 0.1),
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
        type_name: hex(0x0b8f68),
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

/// Responsive breakpoints, measured against the reader's available width.
pub mod breakpoint {
    use gpui::{Pixels, px};

    /// Below this, margin notes fold under their paragraph.
    pub const MARGIN_FOLDS: Pixels = px(1100.0);
    /// Below this, the shelf becomes the kspine.
    pub const SHELF_SPINE: Pixels = px(900.0);
    /// Below this, the spine becomes an on-request overlay.
    pub const SPINE_OVERLAY: Pixels = px(640.0);
    /// At or below this, the rose becomes a list and the thread keeps only "here".
    pub const NARROWEST: Pixels = px(480.0);
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
