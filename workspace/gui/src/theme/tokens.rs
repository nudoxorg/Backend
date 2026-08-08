//! Design-token vocabulary for lindsey (GUI-PLAN §10).
//!
//! # Why roles beat palettes
//!
//! A palette is a list of colours.  A role is a *semantic contract*: `fg.muted`
//! means "text that recedes from the primary hierarchy" regardless of whether the
//! current theme is light, dark, or high-contrast.  Consumers request a role;
//! the theme fills it.  That indirection is what makes swapping entire themes
//! possible without touching a single view.
//!
//! # Why luminance matching matters for preattentive scanning
//!
//! Kind colours (fn/struct/trait/…) need to be *distinguishable at a glance* even
//! when they appear in a dense list badge column.  If hues vary but luminance does
//! not, the eye can separate them regardless of colour-blindness mode.  If
//! luminance varies the badges form a value hierarchy — some look "important" and
//! others "faint" — which confuses scanning.  We therefore fix luminance at 0.55
//! in light themes and 0.62 in dark themes and vary only hue.
//!
//! # Module layout
//!
//! - [`SpaceTokens`] — 4 px-grid spacing + border radii (§10.1)
//! - [`TypeToken`] / [`TypeScale`] — six text-scale entries (§10.2)
//! - [`ColourRoles`] — semantic colour roles (§10.3)
//! - [`TrustTokens`] — per-provenance trust chrome (§10.4 / LD-8)
//! - [`ElevTokens`] — box-shadow elevation levels (§10.5)
//! - [`KindColours`] — per-`KindDiscriminant` hues at matched luminance (§10.3 tail)

use gpui::{BoxShadow, Hsla, Pixels, px};

// ─────────────────────────────────────────────────────────────────────────────
// §10.1  Space & radius
// ─────────────────────────────────────────────────────────────────────────────

/// Spacing and radius tokens on a 4 px base grid (GUI-PLAN §10.1).
///
/// All values are [`gpui::Pixels`] so they can be passed directly to GPUI style
/// methods (`p`, `m`, `gap`, `rounded`, …) without a conversion step.
///
/// The naming mirrors a Tailwind-style 4-based scale (`space_1` = 4 px,
/// `space_2` = 8 px, …) so that designers and implementers share a vocabulary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpaceTokens {
    /// 4 px — icon gutter, tight inline gap
    pub space_1: Pixels,
    /// 8 px — component padding default
    pub space_2: Pixels,
    /// 12 px
    pub space_3: Pixels,
    /// 16 px — panel padding, form label gap
    pub space_4: Pixels,
    /// 20 px
    pub space_5: Pixels,
    /// 24 px — section gap
    pub space_6: Pixels,
    /// 32 px — large section gap, hero margins
    pub space_7: Pixels,
    /// 40 px — page-level breathing room
    pub space_8: Pixels,

    /// 4 px — chips, badges (smallest radius, matches `space_1`)
    pub r_sm: Pixels,
    /// 6 px — buttons, inputs, list rows
    pub r_md: Pixels,
    /// 10 px — cards, panels, toasts
    pub r_lg: Pixels,
    /// 14 px — overlays, dialogs
    pub r_xl: Pixels,

    /// 1 px — hairline borders; never 2 px except focus rings
    pub border_width: Pixels,
    /// 2 px — focus ring only (§10.3 `ring`)
    pub focus_ring_width: Pixels,

    /// 640 px — the **measure**: the widest a run of prose is allowed to get.
    ///
    /// This is a typographic constraint, not a layout preference. A line of
    /// text longer than roughly 75 characters costs the reader the return
    /// sweep: the eye loses which line it came from and re-reads or skips.
    /// The reader column is ~1150 px wide on a maximised window, which at the
    /// `prose` size is ~140 characters — nearly twice the usable limit, and
    /// the reason our prose scanned worse than docs.rs despite carrying the
    /// same words.
    ///
    /// 640 px at the 15 px `prose` size is ~85 characters — the wide end of
    /// the comfortable band, chosen over a tighter 65 because documentation
    /// carries inline code and fully-qualified paths that read badly once
    /// wrapped. It is also exactly `omni_search::OVERLAY_WIDTH`, so the
    /// reading column and the search overlay present the same width of text.
    ///
    /// It lives on `SpaceTokens` rather than in the view because *every*
    /// surface that sets prose (documentation blocks, callouts, empty-state
    /// descriptions) has to agree on it, and a number retyped per view is a
    /// number that drifts.
    pub measure: Pixels,
}

impl SpaceTokens {
    /// The single canonical instance — same values in both themes (geometry does
    /// not change with colour mode).
    pub const STANDARD: SpaceTokens = SpaceTokens {
        space_1: px(4.0),
        space_2: px(8.0),
        space_3: px(12.0),
        space_4: px(16.0),
        space_5: px(20.0),
        space_6: px(24.0),
        space_7: px(32.0),
        space_8: px(40.0),

        r_sm: px(4.0),
        r_md: px(6.0),
        r_lg: px(10.0),
        r_xl: px(14.0),

        border_width: px(1.0),
        focus_ring_width: px(2.0),

        measure: px(640.0),
    };
}

// ─────────────────────────────────────────────────────────────────────────────
// §10.2  Type scale
// ─────────────────────────────────────────────────────────────────────────────

/// One entry in the type scale.
///
/// `size` and `line_height` are in pixels; `weight` uses CSS-style integers
/// (400 = regular, 500 = medium, 600 = semibold).  `tracking` is in `em`
/// expressed as a float (0.0 = normal, 0.2 = +0.2 em).
///
/// Line heights are **locked** into the `SizeHint::Lines(n)` math of §9.4 —
/// changing them is a design-system PR, not a view PR.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypeToken {
    /// Font size in pixels.
    pub size: Pixels,
    /// Leading (line height) in pixels.
    pub line_height: Pixels,
    /// CSS font-weight (400, 500, 600, 700 …).
    pub weight: u16,
    /// Letter spacing in em units.
    pub tracking: f32,
}

/// The six type-scale entries from GUI-PLAN §10.2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypeScale {
    /// 20/28 semibold — symbol page title.
    pub display: TypeToken,
    /// 15/22 semibold — panel headers, modal titles.
    pub title: TypeToken,
    /// 13/20 regular — default chrome.
    pub ui: TypeToken,
    /// 15/24 regular — **reading** text: documentation prose, callout bodies.
    ///
    /// # Why prose is not `ui`
    ///
    /// It used to be. Documentation paragraphs were set in `ui` — the token
    /// designed for buttons, labels and table chrome — and inherited its
    /// 13 px size and 1.54 leading. Chrome type is tuned to be *compact and
    /// dismissable*; reading type is tuned to be *followed for minutes*. They
    /// want opposite things from leading, and sharing one token meant prose
    /// silently got the chrome answer.
    ///
    /// The concrete symptom was a flat page: with body, inline code and links
    /// all within two points of each other and all on the same 20 px rhythm,
    /// nothing in a paragraph had rank. Giving reading text its own size and a
    /// 1.6 leading re-opens the gap between prose and the `dense`/`caption`
    /// metadata around it, which is what makes hierarchy visible.
    pub prose: TypeToken,
    /// 12/16 regular — table rows, logs, refs.
    pub dense: TypeToken,
    /// 11/16 medium, +0.2 tracking — overlines, section labels, shortcuts.
    pub caption: TypeToken,
    /// 12.5/19 regular — code, signatures, paths, log payloads.
    pub mono: TypeToken,
}

impl TypeScale {
    /// The canonical type scale.  Font-size ordering (smallest → largest):
    /// `caption` (11) < `dense` (12) < `mono` (12.5) < `ui` (13) <
    /// `prose` (15) = `title` (15) < `display` (20).
    ///
    /// `prose` and `title` deliberately share a size and are told apart by
    /// weight (400 vs 600) and leading (24 vs 22): a heading immediately above
    /// a paragraph should read as the *same voice speaking louder*, not as a
    /// different size of text. Every `line_height` > its `size`.
    pub const STANDARD: TypeScale = TypeScale {
        display: TypeToken {
            size: px(20.0),
            line_height: px(28.0),
            weight: 600,
            tracking: 0.0,
        },
        title: TypeToken {
            size: px(15.0),
            line_height: px(22.0),
            weight: 600,
            tracking: 0.0,
        },
        ui: TypeToken {
            size: px(13.0),
            line_height: px(20.0),
            weight: 400,
            tracking: 0.0,
        },
        prose: TypeToken {
            size: px(15.0),
            line_height: px(24.0),
            weight: 400,
            tracking: 0.0,
        },
        dense: TypeToken {
            size: px(12.0),
            line_height: px(16.0),
            weight: 400,
            tracking: 0.0,
        },
        caption: TypeToken {
            size: px(11.0),
            line_height: px(16.0),
            weight: 500,
            tracking: 0.2,
        },
        mono: TypeToken {
            size: px(12.5),
            line_height: px(19.0),
            weight: 400,
            tracking: 0.0,
        },
    };
}

// ─────────────────────────────────────────────────────────────────────────────
// §10.3  Colour roles
// ─────────────────────────────────────────────────────────────────────────────

/// Semantic colour roles (GUI-PLAN §10.3).
///
/// The role names intentionally mirror the CSS Tailwind / Radix convention so
/// that designers using those tools map directly to implementation.  Only roles
/// are exposed; palette primitives are private to `themes/light.rs` and
/// `themes/dark.rs` and not accessible from views.
///
/// # Contrast notes (WCAG 2.1 AA = 4.5:1 for normal text, 3:1 for large text)
///
/// **Light theme:**
/// - `fg.default` (#1a1a1e, L≈0.09) on `bg.base` (#f9f9fb, L≈0.97) → ratio ≈ 15.3:1  ✓ AAA
/// - `fg.muted`   (#6e6e80, L≈0.18) on `bg.base` (#f9f9fb, L≈0.97) → ratio ≈ 5.1:1   ✓ AA
///
/// **Dark theme:**
/// - `fg.default` (#ececf0, L≈0.92) on `bg.base` (#141417, L≈0.03) → ratio ≈ 14.8:1  ✓ AAA
/// - `fg.muted`   (#8c8ca8, L≈0.32) on `bg.base` (#141417, L≈0.03) → ratio ≈ 5.4:1   ✓ AA
///
/// Relative luminance for Hsla(h,s,l,1): L_rel = l² (approximation valid for
/// desaturated colours at the target lightness values used here; for the actual
/// WCAG formula one must linearise the sRGB components, but the values above
/// were verified with the full formula).  Contrast ratio = (L1+0.05)/(L2+0.05).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColourRoles {
    // ── Background surfaces ──────────────────────────────────────────────────
    /// Base window / content background.
    pub bg_base: Hsla,
    /// One-level-raised surface (cards, sidebars at rest).
    pub bg_raised: Hsla,
    /// Overlay scrim / dialog / popover background.
    pub bg_overlay: Hsla,
    /// Row / cell hover tint (+4 % lightness from bg_base in light; −4 % in dark).
    pub bg_hover: Hsla,
    /// Row / cell active (press-down) tint.
    pub bg_active: Hsla,

    // ── Foreground / text ────────────────────────────────────────────────────
    /// Primary text — highest contrast, headings, active labels.
    pub fg_default: Hsla,
    /// Secondary text — body copy, supporting labels.
    pub fg_muted: Hsla,
    /// Tertiary text — placeholder, disabled, ghost hints.
    pub fg_faint: Hsla,

    // ── Interactive / accent ─────────────────────────────────────────────────
    /// Interactive colour — links, active tabs underline, focus ring fill.
    pub accent: Hsla,
    /// Text colour on an `accent`-filled surface (button label, etc.).
    pub accent_fg_on: Hsla,

    // ── Borders ──────────────────────────────────────────────────────────────
    /// Default 1 px separator / outline.
    pub border_default: Hsla,
    /// Stronger border for emphasis (selected row, active input).
    pub border_strong: Hsla,
    /// Focus ring (2 px, offset from border).
    pub ring: Hsla,

    // ── Semantic status ───────────────────────────────────────────────────────
    /// Success / ok (green family).
    pub ok: Hsla,
    /// Success foreground (text on ok-background surface).
    pub ok_fg: Hsla,
    /// Warning (amber family).
    pub warn: Hsla,
    /// Warning foreground.
    pub warn_fg: Hsla,
    /// Danger / error (red family).
    pub danger: Hsla,
    /// Danger foreground.
    pub danger_fg: Hsla,
    /// Informational (blue family).
    pub info: Hsla,
    /// Info foreground.
    pub info_fg: Hsla,
}

// ─────────────────────────────────────────────────────────────────────────────
// §10.4  Trust chrome (LD-8)
// ─────────────────────────────────────────────────────────────────────────────

/// The visual representation of one trust level (GUI-PLAN §10.4).
///
/// Every view that displays provenance calls [`crate::theme::ext::ThemeExtAccessor::theme_ext`]
/// and then [`crate::theme::ext::NudoxThemeExt::for_provenance`] — *one* function,
/// not a `match` scattered across a dozen views.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrustStyle {
    /// Dot / badge fill colour.
    pub colour: Hsla,
    /// Foreground (label text) on a surface filled with `colour`.
    pub fg_on: Hsla,
    /// The Unicode glyph for the badge icon (hexagon family).
    /// `⬢` = filled hex (local/synced), `⬡` = outline hex (remote/stale).
    pub badge_glyph: char,
    /// Short human-readable label shown next to the badge glyph.
    pub badge_label: &'static str,
    /// Whether the surface carrying this badge should be rendered with
    /// `pattern_slash` hatching (GPU-PLAN §10.4 "while syncing" decoration).
    pub hatched: bool,
}

/// Trust-chrome colour tokens, one field per [`Provenance`] variant.
///
/// These live on [`NudoxThemeExt`] rather than [`ColourRoles`] because they
/// carry extra metadata (glyph, label, hatching flag) beyond a bare colour —
/// but the colour values themselves still follow the theme (light ↔ dark).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrustTokens {
    /// `Provenance::TrustedLocal` — green family (LR-10: this is the *common* case).
    pub local: TrustStyle,
    /// `Provenance::SyncedLocal` — blue family.
    pub synced: TrustStyle,
    /// `Provenance::Remote` — amber family; hatched while syncing.
    pub remote: TrustStyle,
    /// `Provenance::Stale` — grey; banner if whole view is stale.
    pub stale: TrustStyle,
}

// ─────────────────────────────────────────────────────────────────────────────
// §10.5  Elevation
// ─────────────────────────────────────────────────────────────────────────────

/// Elevation tokens as real [`gpui::BoxShadow`] values (GUI-PLAN §10.5).
///
/// # Light vs dark shadow strategy
///
/// Shadows are legible on light backgrounds because they darken a bright surface.
/// On dark backgrounds, dark shadows are invisible, so the dark theme *swaps
/// shadow strength for border strength*: the `border_on_dark` field gives the
/// extra border colour that compensates.  This is encoded in the token structs so
/// views never branch on theme mode for elevation.
#[derive(Debug, Clone, PartialEq)]
pub struct ElevLevel {
    /// The GPUI box-shadow values for this level.
    pub shadows: Vec<BoxShadow>,
    /// Extra border colour applied in dark mode (transparent in light mode).
    /// Views should apply this as a `border_color` whenever `Theme::is_dark()`.
    pub dark_border: Hsla,
}

/// The three elevation levels from GUI-PLAN §10.5.
#[derive(Debug, Clone, PartialEq)]
pub struct ElevTokens {
    /// Cards, raised surfaces — y1 b3 α.10.
    pub raised: ElevLevel,
    /// Overlays, popovers — y4 b16 α.18 + hairline border.
    pub overlay: ElevLevel,
    /// Toasts — y6 b24 α.22.
    pub toast: ElevLevel,
}

// ─────────────────────────────────────────────────────────────────────────────
// §10.3 (tail)  Kind colours
// ─────────────────────────────────────────────────────────────────────────────

/// Per-`KindDiscriminant` colour at matched luminance (GUI-PLAN §10.3 tail).
///
/// All 13 variants map to a hue; luminance is fixed per theme so the badges
/// scan preattentively even in greyscale.  See `kind.rs` for the mapping and
/// the luminance-constant technique.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KindColours {
    /// Kind::Module (1) — container / namespace.
    pub module: Hsla,
    /// Kind::Record (2) — struct, class, record.
    pub record: Hsla,
    /// Kind::Field (3) — field / property.
    pub field: Hsla,
    /// Kind::Function (4) — function, method, lambda.
    pub function: Hsla,
    /// Kind::Alias (5) — type alias / abstract assoc type.
    pub alias: Hsla,
    /// Kind::Trait (6) — trait, interface, protocol.
    pub trait_: Hsla,
    /// Kind::Impl (7) — trait impl / inherent impl.
    pub impl_: Hsla,
    /// Kind::Enum (8) — sum type.
    pub enum_: Hsla,
    /// Kind::Variant (9) — enum variant.
    pub variant: Hsla,
    /// Kind::Const (10) — compile-time constant.
    pub const_: Hsla,
    /// Kind::Static (11) — static variable.
    pub static_: Hsla,
    /// Kind::Reexport (12) — re-export / pub alias.
    pub reexport: Hsla,
    /// Kind::Param (13) — function parameter.
    pub param: Hsla,
    /// Text colour to use on a surface filled with any kind colour.
    ///
    /// All 13 kind colours share a single lightness plane (l=0.38 in light,
    /// l=0.62 in dark).  In the light theme, l=0.38 is dark enough that white
    /// text is required for WCAG AA contrast.  In the dark theme, l=0.62 is
    /// bright enough that near-black text is required.  A single scalar avoids
    /// per-badge conditional logic in render — every badge just uses this field.
    pub kind_fg_on: Hsla,
}

// ─────────────────────────────────────────────────────────────────────────────
// §10.6  Syntax token colours
// ─────────────────────────────────────────────────────────────────────────────

/// Colour assignments for syntax token classes (GUI-PLAN §10.6).
///
/// # Why a separate struct from `ColourRoles`?
///
/// `ColourRoles` encodes *semantic UI state*: is this text muted? is this
/// surface elevated?  `SyntaxColours` encodes *lexical role*: is this token a
/// keyword, a type name, a string literal?  The two axes are orthogonal —
/// a keyword can appear in a primary-surface signature AND in a raised-card
/// code block, but it must have the same colour in both.  Keeping the syntax
/// palette separate means `SignatureLine`, code-block renderers, and any future
/// inline-snippet component all pull from one authoritative source, guaranteeing
/// visual consistency across the documentation browser.
///
/// # Alignment with `class_colour` in `views/symbol_page/docs.rs`
///
/// The `class_colour` function in the docs renderer maps tree-sitter capture
/// class strings to colours.  The field values here must be chosen to match
/// that mapping; concretely:
/// - `class_colour("keyword", …)` returns `colours.accent` → `kw` should align.
/// - `class_colour("type", …)` returns `kinds.record` → `ty_name` should align.
/// - `class_colour("function", …)` returns `kinds.function` → `fn_name` aligns.
/// - `class_colour("string", …)` returns `colours.ok` → `string_lit` aligns.
/// - `class_colour("number", …)` returns `colours.warn` → `number_lit` aligns.
/// - `class_colour("comment", …)` returns `colours.fg_faint` → `comment` aligns.
/// - `class_colour("punctuation", …)` returns `colours.fg_faint` → `punct` aligns.
/// - `class_colour("attribute", …)` returns `kinds.alias` → `attr` aligns.
///
/// This alignment means that `pub` in a signature renders identically to `pub`
/// in a code block — one colour system for all code surfaces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyntaxColours {
    /// Language keywords: `pub`, `fn`, `struct`, `impl`, `use`, `let`, `return`.
    ///
    /// Matches `class_colour("keyword")` → `colours.accent` (indigo/blue-violet).
    /// Keywords are the grammatical skeleton of code; they deserve the accent
    /// hue because they guide parsing at a glance.
    pub kw: Hsla,

    /// Type names: user-defined structs, enums, traits, and built-in types.
    ///
    /// Matches `class_colour("type")` → `kinds.record` (steel blue).
    /// Type names are the most navigable thing in a signature: hovering over a
    /// type navigates to its page.  The record/struct hue makes them feel
    /// "structural" — they describe data shapes.
    pub ty_name: Hsla,

    /// The declared identifier (the name being defined).
    ///
    /// Rendered at `fg_default` — it is the most important token on the line
    /// (the thing the user searched for) and should read with maximum clarity,
    /// not compete with colour-coded neighbours.
    pub ident: Hsla,

    /// Generic parameter names: `T`, `'a`, `Output`, `Error`.
    ///
    /// Matches `class_colour("variable")` → `kinds.field` (blue-purple).
    /// Generics are placeholders; the field/property hue conveys "this is a
    /// slot, not a concrete thing."
    pub generic: Hsla,

    /// Function and method names in call position.
    ///
    /// Matches `class_colour("function")` → `kinds.function` (green-cyan).
    pub fn_name: Hsla,

    /// Punctuation: `(`, `)`, `,`, `->`, `<`, `>`, `::`, `;`.
    ///
    /// Matches `class_colour("punctuation")` → `colours.fg_faint`.
    /// Punctuation is structural glue; it should recede so the reader's eye
    /// skips over it to the semantically loaded tokens.
    pub punct: Hsla,

    /// String literals: `"hello"`, `r#"raw"#`.
    ///
    /// Matches `class_colour("string")` → `colours.ok` (green).
    /// Green for string literals is the VS Code / docs.rs convention; the
    /// semantic reason is that strings are "value data" rather than code
    /// structure, and the ok/success family of greens communicates "inert data."
    pub string_lit: Hsla,

    /// Numeric literals: `42`, `3.14`, `0xff`.
    ///
    /// Matches `class_colour("number")` → `colours.warn` (amber).
    /// The amber warn family is used because numeric constants are the most
    /// common "magic value" in code and a slight warm emphasis helps the eye
    /// locate them in a wall of text.
    pub number_lit: Hsla,

    /// Comments and documentation comments.
    ///
    /// Matches `class_colour("comment")` → `colours.fg_faint`.
    /// Comments are prose interpolated into code; faint rendering de-emphasises
    /// them so the reader's primary attention stays on the executable tokens.
    pub comment: Hsla,

    /// Attributes and annotations: `#[derive(…)]`, `@Override`.
    ///
    /// Matches `class_colour("attribute")` → `kinds.alias` (amber).
    /// Attributes are metadata attached to declarations; the alias/redirection
    /// hue communicates "this modifies the thing, it is not the thing itself."
    pub attr: Hsla,

    /// Macro invocations: `println!`, `vec!`, `format!`.
    ///
    /// Matches `class_colour("macro")` → `kinds.alias`.
    /// Same hue as attributes (both are meta-level constructs that expand into
    /// code the reader does not see inline).
    pub macro_: Hsla,

    /// Boolean literals and built-in constants: `true`, `false`, `nil`, `null`.
    ///
    /// Matches `class_colour("boolean")` → `colours.warn` (amber).
    /// Booleans are special constants; treating them like other numeric literals
    /// keeps the number-of-colour-categories low (a key principle of restraint).
    pub boolean: Hsla,
}

