//! The Docs tab — §9.4's zero-jump guarantee, made real.
//!
//! # The one idea in this file
//!
//! **The list is laid out before the content exists, and it is never re-laid
//! out.** `SymbolHead::section_plan` says how many sections are coming and how
//! tall each one will be. From that we build, in a single frame:
//!
//! * a `ListState` spliced *once* to exactly `section_plan.len()` items, and
//! * a `Vec<SectionSlot>` where slot `i` is permanently bound to plan entry `i`.
//!
//! A section arriving is therefore a **replacement at a fixed index**, never an
//! insertion. The item count never changes for the life of a generation, so
//! `ListState`'s logical scroll anchor — a `(item_ix, offset_in_item)` pair, not
//! a pixel offset — stays valid no matter what arrives above or below the
//! reader. That is the structural half of the guarantee.
//!
//! The dimensional half is [`reserved_height`]: the skeleton and the arrived
//! content are laid out at the *same* reserved height, computed once from
//! `SizeHint` (§9.4.1 — `Lines(n) → n × line_height`, `Rows(n) → n × row_height`,
//! `Unknown → 3 lines`). Content sits in a `min_h(reserved)` box, so a section
//! can only grow past its reserve, never shrink below it. And when a slot's
//! content lands we call `ListState::remeasure_items(i..i+1)`, which — unlike
//! `splice` — keeps the anchor and, if the anchored item is the one being
//! remeasured, pins an absolute pending scroll (`list.rs:374`). Nothing the
//! reader is looking at can move.
//!
//! Code blocks get the same treatment one level down (§9.4.2): the block is
//! `line_count × mono.line_height` tall with `whitespace_nowrap`, so wrapping
//! cannot add a line, and `Highlight` arriving only ever swaps
//! `HighlightStyle::color` on byte ranges of a single `StyledText`. Mono metrics
//! are colour-invariant, so `highlight.sweep` is provably geometry-free.
//!
//! # Why inline runs are one `StyledText` and not a row of `div`s
//!
//! A paragraph rendered as a flex-wrap row of per-run `div`s cannot wrap *inside*
//! a run — you get ragged text that reflows differently at every window width.
//! Instead every paragraph is concatenated at projection time into one
//! `SharedString` plus a list of `(byte range, style)` pairs. `InteractiveText`
//! then gives real inline flow *and* per-range click targets, which is how every
//! `InlineRun::Link` becomes a live link.
//!
//! # LD-7
//!
//! `RenderSection`, `ProseBlock` and `InlineRun` are all `#[non_exhaustive]`.
//! Every match here has a fallback arm that renders a visible chip. Nothing is
//! dropped silently and nothing panics.

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    AnyElement, App, ElementId, HighlightStyle, Hsla, InteractiveElement as _, InteractiveText,
    IntoElement, ListAlignment, ListState, ParentElement, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled, StyledText, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::{StyledExt as _, skeleton::Skeleton, tooltip::Tooltip};
use nudox_engine::wire::{
    CalloutLevel, FieldRow, HighlightSpan, InlineRun, LinkOrigin, LinkRepairKind, LinkTarget,
    MemberRow, ProseBlock, RenderSection, SectionId, SectionKind, SectionPlan, SizeHint, SymbolKey,
};

use crate::motion::color::MotionColor;
use crate::motion::declarative::{entrance_id, rise_in};
use crate::motion::spring::Spring;
use crate::motion::tokens::{MotionTokens, ROW_CASCADE_WINDOW};
use crate::theme::ext::ThemeExtAccessor as _;
use crate::theme::tokens::{ColourRoles, KindColours, SyntaxColours};
use crate::ui::{Badge, SigToken, SignatureLine};

use super::header::{KindChip, link_ix, shared, sig_tokens};

/// Extra vertical slack folded into every reserve.
///
/// `section.arrive` is `rise_in`, which animates a 6 px `margin-top` down to
/// zero. Without slack that 6 px would briefly push a section past its reserve
/// and force a re-measure on every frame of the entrance. One space token of
/// headroom absorbs it, so the entrance costs nothing.
const RISE_SLACK_TOKENS: f32 = 1.0;

/// How many skeleton bars we are willing to draw for one section.
///
/// A 400-line prose section does not need 400 shimmer bars to read as "text is
/// coming"; past a couple of dozen the effect is noise and the cost is real.
const MAX_SKELETON_BARS: usize = 24;

// ─────────────────────────────────────────────────────────────────────────────
// Geometry
// ─────────────────────────────────────────────────────────────────────────────

/// The line/row metrics §9.4's size-hint arithmetic is written against.
///
/// Read once from the theme when a plan lands. §10.2 notes these line heights
/// are *locked* into this arithmetic — changing them is a design-system change,
/// not a view change.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    prose_line: Pixels,
    mono_line: Pixels,
    row: Pixels,
    pad_y: Pixels,
    slack: Pixels,
}

impl Metrics {
    /// Sample the current theme.
    pub fn from_theme(cx: &App) -> Self {
        let ext = cx.theme_ext();
        let ts = ext.type_scale;
        let sp = ext.space;
        Self {
            // Reading text is set in `prose`, not `ui`. §10.2 notes these line
            // heights are locked into the arithmetic below — which is exactly
            // why the token swap has to happen *here* as well as in the
            // template. Reserve a section at the chrome leading and render it
            // at the reading leading and every section is short by
            // `(24 − 20) × lines`, which is the zero-jump guarantee failing
            // silently on every paragraph.
            prose_line: ts.prose.line_height,
            mono_line: ts.mono.line_height,
            // A member/field row is one dense line plus a hairline of padding
            // above and below — the docs.rs table rhythm.
            row: ts.dense.line_height + sp.space_1 + sp.space_1,
            pad_y: sp.space_3 + sp.space_3,
            slack: sp.space_2 * RISE_SLACK_TOKENS,
        }
    }
}

/// §9.4.1 verbatim: the height a section is promised before it exists.
///
/// The *same* value is used for the skeleton and as the arrived content's
/// `min_h`, which is what makes arrival a replacement rather than a reflow.
pub fn reserved_height(kind: SectionKind, hint: SizeHint, m: &Metrics) -> Pixels {
    let unit = match kind {
        SectionKind::CodeBlock | SectionKind::Examples => m.mono_line,
        _ => m.prose_line,
    };
    let body = match hint {
        SizeHint::Lines(n) => unit * (n.max(1) as f32),
        SizeHint::Rows(n) => m.row * (n.max(1) as f32),
        // §9.4.1: no estimate → a three-line block. Never zero: a zero-height
        // skeleton collapses the list and hands the reader a jump on arrival.
        _ => unit * 3.0,
    };
    body + m.pad_y + m.slack
}

/// The default outline label for a section whose content has not arrived.
///
/// This is why our sidebar exists before the document does.
fn kind_label(kind: SectionKind) -> SharedString {
    SharedString::from(match kind {
        SectionKind::Prose => "Documentation",
        SectionKind::CodeBlock => "Code",
        SectionKind::Members => "Members",
        SectionKind::Fields => "Fields",
        SectionKind::Examples => "Examples",
        SectionKind::Callout => "Note",
        _ => "Section",
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-ready projections
// ─────────────────────────────────────────────────────────────────────────────

/// How one byte range of a paragraph is painted.
///
/// `pub` because it is half of the render-ready projection: `RichText` says
/// *which* bytes, this says *how*, and `run_highlight_style` turns it into a
/// `HighlightStyle`. The screenshot plane needs all three to rasterise a real
/// paragraph without re-deriving any of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStyle {
    Text,
    Code,
    Strong,
    Em,
    Link,
    /// A link whose spelling we repaired (`LinkOrigin::Repaired`). Same accent
    /// hue as a real link — it *is* a real link — but a wavy `warn` underline,
    /// so a reader can tell at a glance which links the crate author wrote and
    /// which ones we inferred.
    RepairedLink,
    /// A run variant this binary does not understand (LD-7).
    Unknown,
}

/// The reader-facing name of a link repair, for the legend beside a marked
/// link.
///
/// # Why this function exists in the GUI at all
///
/// It is the fifth compile error a new `LinkRepairKind` costs, and the only one
/// that lands in *this* package. `LinkRepairKind` is deliberately exhaustive
/// (see its docs in `nudox-engine`), so the match below does not compile until
/// a new repair has been given a name a reader can understand. A repair that
/// ships without a reader affordance is a silent repair, which is the whole
/// thing this design exists to prevent.
///
/// The *sentence* a reader sees is still `LinkRepair::note`, rendered in the
/// chunker (LR-3). This is only the short label, and it does no string work —
/// every arm returns a `&'static str`.
pub(crate) fn repair_legend_label(kind: LinkRepairKind) -> SharedString {
    match kind {
        LinkRepairKind::TransposedOpenDelimiter => {
            SharedString::new_static("repaired link — transposed backtick and bracket")
        }
    }
}

/// The paint for one run style, given the current theme.
///
/// # Why this is a free function and not an inline closure
///
/// It used to be a `match` buried inside `render_rich`'s style-mapping
/// closure, which meant the only way to observe what a run style *looks like*
/// was to render a whole document. That made the one thing worth asserting
/// about `RunStyle::RepairedLink` — that it is visibly different from
/// `RunStyle::Link` in real pixels — untestable without duplicating the match,
/// and a duplicated style map is a claim that rots (doctrine §8).
///
/// Lifting it out costs nothing at the call site and gives the screenshot
/// plane something real to rasterise. There is exactly one definition of what
/// a run looks like, and both the app and the evidence read it.
pub fn run_highlight_style(
    style: RunStyle,
    colours: &ColourRoles,
    border_width: Pixels,
) -> HighlightStyle {
    match style {
        // Plain body text carries the primary foreground: it is
        // the thing the reader came for. Everything else in the
        // run vocabulary is now positioned *relative to this*
        // rather than above it.
        RunStyle::Text => HighlightStyle {
            color: Some(colours.fg_default),
            ..Default::default()
        },
        // Inline code differentiates by *surface*, not by
        // luminance. It used to be the only run brighter than the
        // sentence around it, which inverted the hierarchy — a
        // type name mentioned in passing read as more important
        // than the sentence explaining it. The tinted chip already
        // says "this is code"; it does not also need to shout.
        RunStyle::Code => HighlightStyle {
            color: Some(colours.fg_default),
            background_color: Some(colours.bg_hover),
            ..Default::default()
        },
        // Strong is now the *only* run that can go heavier than
        // body, so bold means something again.
        RunStyle::Strong => HighlightStyle {
            color: Some(colours.fg_default),
            font_weight: Some(gpui::FontWeight::BOLD),
            ..Default::default()
        },
        RunStyle::Em => HighlightStyle {
            color: Some(colours.fg_default),
            font_style: Some(gpui::FontStyle::Italic),
            ..Default::default()
        },
        RunStyle::Link => HighlightStyle {
            color: Some(colours.accent),
            underline: Some(gpui::UnderlineStyle {
                thickness: border_width,
                color: Some(colours.accent),
                wavy: false,
            }),
            ..Default::default()
        },
        // Same accent hue as `Link` — a repaired link is a real,
        // working link and must not be demoted. The difference is
        // the underline: wavy, in `warn`, which is the
        // spell-checker idiom for "this is not how the source
        // spells it". The marker never touches the *text*, so
        // selection, copy, and every text assertion stay honest.
        RunStyle::RepairedLink => HighlightStyle {
            color: Some(colours.accent),
            underline: Some(gpui::UnderlineStyle {
                thickness: border_width,
                color: Some(colours.warn),
                wavy: true,
            }),
            ..Default::default()
        },
        RunStyle::Unknown => HighlightStyle {
            color: Some(colours.warn),
            ..Default::default()
        },
    }
}

/// Where a clickable range in a paragraph goes.
#[derive(Clone, Debug)]
enum LinkDest {
    Symbol(SymbolKey),
    Url(SharedString),
}

/// One paragraph, heading, or list item: a single string plus styling.
///
/// `pub` for the same reason as [`RunStyle`]: this is the projection the
/// renderer consumes, so it is also the projection the evidence must consume.
/// The fields stay private — [`RichText::text`] and [`RichText::styles`] are
/// the read-only view, so nothing outside can build a `RichText` whose ranges
/// do not address its own string.
#[derive(Clone, Debug)]
pub struct RichText {
    text: SharedString,
    styles: Arc<[(Range<usize>, RunStyle)]>,
    link_ranges: Arc<[Range<usize>]>,
    links: Arc<[LinkDest]>,
    /// Tooltip text per repaired-link range, keyed by the same byte ranges
    /// `link_ranges` uses. Empty when the page has no repairs.
    ///
    /// The strings are the engine's `LinkRepair::note` **verbatim** — lindsey
    /// does no string work (LR-3), so what a reader is told about a repair is
    /// authored in exactly one place.
    repair_notes: Arc<[(Range<usize>, SharedString)]>,
}

impl RichText {
    /// The shaped string every range in [`Self::styles`] addresses.
    pub fn text(&self) -> &SharedString {
        &self.text
    }

    /// The `(byte range, style)` pairs, contiguous and in order.
    pub fn styles(&self) -> &[(Range<usize>, RunStyle)] {
        &self.styles
    }

    /// Flatten inline runs into one shaped string. Projection time only.
    pub fn from_runs(runs: &[InlineRun]) -> Self {
        let mut text = String::new();
        let mut styles: Vec<(Range<usize>, RunStyle)> = Vec::new();
        let mut link_ranges: Vec<Range<usize>> = Vec::new();
        let mut links: Vec<LinkDest> = Vec::new();
        let mut repair_notes: Vec<(Range<usize>, SharedString)> = Vec::new();

        for run in runs {
            let start = text.len();
            let style = match run {
                InlineRun::Text { text: s } => {
                    text.push_str(s);
                    RunStyle::Text
                }
                InlineRun::Code { text: s } => {
                    text.push_str(s);
                    RunStyle::Code
                }
                InlineRun::Strong { text: s } => {
                    text.push_str(s);
                    RunStyle::Strong
                }
                InlineRun::Em { text: s } => {
                    text.push_str(s);
                    RunStyle::Em
                }
                InlineRun::Link {
                    text: t,
                    target,
                    origin,
                } => {
                    text.push_str(t);
                    links.push(match target {
                        LinkTarget::Symbol { key } => LinkDest::Symbol(key.clone()),
                        LinkTarget::Url { url: u } => LinkDest::Url(shared(u)),
                        // An unknown target still shows its text; it simply is
                        // not clickable (LD-7 — visible, not silent).
                        _ => LinkDest::Url(SharedString::from("")),
                    });
                    link_ranges.push(start..text.len());
                    // No wildcard arm, unlike every other match in this file:
                    // `LinkOrigin` is deliberately *not* `#[non_exhaustive]`
                    // and has exactly two states by construction (see its docs
                    // in `nudox-engine`). A `_` here would be dead code today
                    // and, if the type ever did grow, would silently paint a
                    // new kind of repair as an ordinary link — the exact
                    // silence this whole feature exists to remove.
                    match origin {
                        LinkOrigin::Authored => RunStyle::Link,
                        LinkOrigin::Repaired(repair) => {
                            repair_notes.push((start..text.len(), shared(&repair.note)));
                            RunStyle::RepairedLink
                        }
                    }
                }
                // LD-7: an unrecognised run becomes a visible chip, never a gap.
                _ => {
                    text.push_str("⟨?⟩");
                    RunStyle::Unknown
                }
            };
            let end = text.len();
            if end > start {
                styles.push((start..end, style));
            }
        }

        Self {
            text: SharedString::from(text),
            styles: Arc::from(styles),
            link_ranges: Arc::from(link_ranges),
            links: Arc::from(links),
            repair_notes: Arc::from(repair_notes),
        }
    }
}

/// A code block, with its optional highlight upgrade and sweep animation.
#[derive(Debug)]
struct CodeView {
    text: SharedString,
    lang: SharedString,
    /// The engine's promised line count — this block's height, fixed (§9.4.2).
    line_count: u32,
    /// Byte ranges with their class index into `classes`. Empty until a
    /// `DocEvent::Highlight` for this section lands.
    spans: Vec<(Range<usize>, usize)>,
    /// Distinct token classes present in this block.
    classes: Vec<SharedString>,
    /// One `highlight.sweep` colour spring per class, present only while the
    /// sweep runs. Dropped on settle, so a finished document ticks nothing.
    sweep: Option<Vec<MotionColor>>,
}

impl CodeView {
    fn new(text: SharedString, lang: SharedString, line_count: u32) -> Self {
        Self {
            text,
            lang,
            line_count,
            spans: Vec::new(),
            classes: Vec::new(),
            sweep: None,
        }
    }

    /// Fixed height: `line_count` mono lines. Colour cannot change this.
    fn height(&self, m: &Metrics) -> Pixels {
        m.mono_line * (self.line_count.max(1) as f32)
    }

    /// Ingest a `Highlight` event and arm the sweep. Projection time only.
    fn apply_spans(
        &mut self,
        spans: &[HighlightSpan],
        colours: &ColourRoles,
        syntax: &SyntaxColours,
        reduced: bool,
    ) {
        let mut classes: Vec<SharedString> = Vec::new();
        let mut out: Vec<(Range<usize>, usize)> = Vec::with_capacity(spans.len());
        let len = self.text.len();

        for span in spans {
            let start = span.start as usize;
            let end = span.end as usize;
            // Defensive: `StyledText::with_highlights` debug-asserts char
            // boundaries, and a producer bug must not become a panic (LD-7).
            if start >= end
                || end > len
                || !self.text.is_char_boundary(start)
                || !self.text.is_char_boundary(end)
            {
                continue;
            }
            let class_ix = match classes.iter().position(|c| c.as_ref() == &*span.class) {
                Some(ix) => ix,
                None => {
                    classes.push(shared(&span.class));
                    classes.len() - 1
                }
            };
            out.push((start..end, class_ix));
        }

        let mut sweep = Vec::with_capacity(classes.len());
        for class in &classes {
            let target = class_colour(class, syntax);
            // §5.2 `highlight.sweep`: every class starts at `fg.muted` — which
            // is exactly what the block already looks like — and fades to its
            // final colour. Nothing moves; only hue.
            let mut motion = MotionColor::new(colours.fg_muted, Spring::SNAPPY);
            if reduced {
                motion.snap_to(target);
            } else {
                motion.animate_to(target);
            }
            sweep.push(motion);
        }

        self.classes = classes;
        self.spans = out;
        self.sweep = if sweep.is_empty() { None } else { Some(sweep) };
    }

    /// Advance the sweep. Returns `true` while it is still moving.
    fn tick(&mut self, now: Instant) -> bool {
        let Some(sweep) = self.sweep.as_mut() else {
            return false;
        };
        let mut moving = false;
        for motion in sweep.iter_mut() {
            moving |= motion.tick(now);
        }
        if !moving {
            // Settled: drop the springs so a finished page costs nothing.
            self.sweep = None;
        }
        moving
    }

    /// The colour a class paints at *this instant*.
    fn class_colour_now(&self, class_ix: usize, syntax: &SyntaxColours) -> Hsla {
        match self.sweep.as_ref().and_then(|s| s.get(class_ix)) {
            Some(motion) => motion.value(),
            None => match self.classes.get(class_ix) {
                Some(class) => class_colour(class, syntax),
                None => syntax.ident,
            },
        }
    }
}

/// Map a highlighter token class onto the design system.
///
/// # Why this reads `SyntaxColours` rather than picking off the other palettes
///
/// A token's colour follows its *lexical role*, which is a third axis beside UI
/// state ([`ColourRoles`]) and symbol kind ([`KindColours`]). Before
/// `SyntaxColours` existed this function and [`crate::ui::signature_line`] each
/// chose their own colours off those two axes — and disagreed: a type name came
/// out steel blue in a code block and sky blue in a signature, so the same word
/// changed colour depending on which surface you read it on.
///
/// Both now read one token set. That is what makes `struct` in a code sample
/// identical to the `struct` badge beside it — cross-surface consistency
/// docs.rs has no way to offer. Adding a class here without a matching
/// `SyntaxColours` field re-opens exactly the drift the type exists to close.
fn class_colour(class: &str, syntax: &SyntaxColours) -> Hsla {
    match class {
        "keyword" | "kw" | "storage" | "keyword.control" => syntax.kw,
        "string" | "str" | "char" | "string.special" => syntax.string_lit,
        "comment" | "comment.doc" | "doc" => syntax.comment,
        "boolean" => syntax.boolean,
        "number" | "constant" | "constant.numeric" => syntax.number_lit,
        "type" | "type.builtin" | "class" | "struct" | "interface" => syntax.ty_name,
        "function" | "function.method" | "method" | "fn" => syntax.fn_name,
        "variable" | "property" | "field" => syntax.generic,
        "attribute" | "annotation" => syntax.attr,
        "macro" => syntax.macro_,
        "punctuation" | "operator" | "delimiter" => syntax.punct,
        // Unknown class: a readable default rather than an invisible one.
        _ => syntax.ident,
    }
}

/// One member or field row, fully render-ready.
#[derive(Clone, Debug)]
struct RowView {
    key: SymbolKey,
    name: SharedString,
    sig: Vec<SigToken>,
    links: Arc<[SymbolKey]>,
    kind: KindChip,
    source: Option<SharedString>,
}

impl RowView {
    fn from_member(row: &MemberRow) -> Self {
        let mut links = Vec::new();
        Self {
            key: row.key.clone(),
            name: shared(&row.name),
            sig: sig_tokens(&row.sig, &mut links),
            links: Arc::from(links),
            kind: KindChip::from_tag(row.kind),
            source: row.source.jump_target().map(SharedString::from),
        }
    }

    fn from_field(row: &FieldRow) -> Self {
        let mut links = Vec::new();
        Self {
            key: row.key.clone(),
            name: shared(&row.name),
            sig: sig_tokens(&row.ty_tokens, &mut links),
            links: Arc::from(links),
            kind: KindChip::from_tag(row.kind),
            source: None,
        }
    }
}

/// A block inside a prose-bearing section.
#[derive(Debug)]
enum BlockView {
    Paragraph(RichText),
    Heading {
        level: u8,
        text: RichText,
    },
    List {
        items: Vec<(SharedString, RichText)>,
    },
    Rule,
    Code(CodeView),
    /// LD-7 fallback for a block variant we do not know.
    Unknown(SharedString),
}

/// A fully-projected section: what actually gets painted.
#[derive(Debug)]
enum SectionView {
    Blocks(Vec<BlockView>),
    Callout {
        level: CalloutLevel,
        blocks: Vec<BlockView>,
    },
    Code(CodeView),
    Rows {
        title: SharedString,
        rows: Vec<RowView>,
    },
    /// LD-7 fallback for a section kind we do not know.
    Unknown(SharedString),
}

// ─────────────────────────────────────────────────────────────────────────────
// Slots
// ─────────────────────────────────────────────────────────────────────────────

/// One planned section: its promised geometry, and its content once it lands.
///
/// A slot is created from `SectionPlan` and *never* moves, is never inserted
/// before, and is never removed. It only ever fills in.
pub struct SectionSlot {
    /// Links this slot to its `RenderSection` and to `Highlight` events.
    pub id: SectionId,
    /// What the plan said would arrive here.
    pub kind: SectionKind,
    /// The promised height (§9.4.1). Applies to skeleton and content alike.
    pub reserved: Pixels,
    /// Outline label — the kind name until the real heading arrives.
    pub label: SharedString,
    /// `true` once `label` came from content rather than from the plan.
    pub label_refined: bool,
    /// When the content landed; drives the `section.arrive` window.
    pub arrived_at: Option<Instant>,
    body: Option<SectionView>,
}

impl SectionSlot {
    /// Whether this slot is still showing its skeleton.
    pub fn is_pending(&self) -> bool {
        self.body.is_none()
    }

    /// How many enumerable rows this section holds, when it holds a table.
    ///
    /// `None` for prose, code and callouts — sections that are read, not
    /// counted. The outline uses it to tell a *generated* section apart from an
    /// authored heading that happens to share its name: `Point`'s doc comment
    /// has a `# Fields` heading and the struct also has a `Fields` table, and
    /// `08-symbol-opened.png` listed both in the rail as the bare word
    /// "Fields", twice, with nothing to choose between them.
    ///
    /// The count is derived from the rows themselves rather than carried
    /// alongside them, so it cannot drift from what the section paints
    /// (doctrine §6's "counted" rule, applied to a label).
    pub fn row_count(&self) -> Option<usize> {
        match self.body.as_ref()? {
            SectionView::Rows { rows, .. } => Some(rows.len()),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DocsBody
// ─────────────────────────────────────────────────────────────────────────────

type OpenHandler = Rc<dyn Fn(&SymbolKey, &mut Window, &mut App)>;
type SourceHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// The Docs tab: a virtualized `list()` of sections over a fixed skeleton.
pub struct DocsBody {
    list: ListState,
    slots: Vec<SectionSlot>,
    metrics: Metrics,
    /// Generation stamp — part of every entrance `ElementId`, so a tab switch
    /// inside one generation never replays an entrance (LD-19).
    generation: u64,
    /// How many sections we have consumed from the store's `Progressive`.
    consumed: usize,
    /// Section ids whose highlights have already been ingested.
    highlighted: Vec<SectionId>,
    /// Where a symbol link goes when clicked.
    on_open: Option<OpenHandler>,
    /// Where a declared source location goes when its copy affordance is clicked.
    on_source: Option<SourceHandler>,
}

impl DocsBody {
    /// An empty body, before any `Head` has landed.
    pub fn new(cx: &App) -> Self {
        let sp = cx.theme_ext().space;
        Self {
            // `Top` alignment: documents read downward. The overdraw keeps the
            // next screenful measured before it is revealed, so fast scrolling
            // never shows an unmeasured gap.
            list: ListState::new(0, ListAlignment::Top, sp.space_8 * 8.0),
            slots: Vec::new(),
            metrics: Metrics::from_theme(cx),
            generation: 0,
            consumed: 0,
            highlighted: Vec::new(),
            on_open: None,
            on_source: None,
        }
    }

    /// Install the symbol-link navigation handler.
    pub fn on_open(&mut self, f: impl Fn(&SymbolKey, &mut Window, &mut App) + 'static) {
        self.on_open = Some(Rc::new(f));
    }

    /// Install the declared-source action handler.
    pub fn on_source(&mut self, f: impl Fn(&SharedString, &mut Window, &mut App) + 'static) {
        self.on_source = Some(Rc::new(f));
    }

    /// The `ListState` the page hands to `list()`.
    pub fn list_state(&self) -> &ListState {
        &self.list
    }

    /// The planned sections — the outline reads this before content exists.
    pub fn slots(&self) -> &[SectionSlot] {
        &self.slots
    }

    /// `true` once a plan has been installed.
    pub fn has_plan(&self) -> bool {
        !self.slots.is_empty()
    }

    /// How many planned sections have arrived.
    pub fn arrived_count(&self) -> usize {
        self.consumed
    }

    // ── The zero-jump entry points ───────────────────────────────────────────

    /// Lay out the whole document from `section_plan`, before any content.
    ///
    /// This is the **only** call that changes the list's item count. Everything
    /// after it is an in-place replacement, which is precisely why the reader's
    /// scroll position cannot be disturbed by streaming.
    pub fn set_plan(&mut self, plan: &[SectionPlan], generation: u64, cx: &App) {
        self.metrics = Metrics::from_theme(cx);
        self.generation = generation;
        self.consumed = 0;
        self.highlighted.clear();
        self.slots = plan
            .iter()
            .map(|p| SectionSlot {
                id: p.id,
                kind: p.kind,
                reserved: reserved_height(p.kind, p.size_hint, &self.metrics),
                label: kind_label(p.kind),
                label_refined: false,
                arrived_at: None,
                body: None,
            })
            .collect();
        // Exactly one splice per generation.
        self.list.reset(self.slots.len());
    }

    /// Project any sections the store has accumulated but we have not.
    ///
    /// Returns `true` if anything changed. Called from the store observation,
    /// never from `render` — all the string work lives here.
    pub fn sync_sections(&mut self, sections: &[RenderSection]) -> bool {
        if self.consumed >= sections.len() {
            return false;
        }
        let now = Instant::now();
        let mut changed = false;

        for section in &sections[self.consumed..] {
            let id = section.section_id();
            let Some(ix) = self.slots.iter().position(|s| s.id == id) else {
                // A section the plan did not predict. Inserting it would move
                // every slot below and break the anchor, so we drop it and
                // leave a trace: the plan is the layout contract.
                tracing::warn!(section = id.0, "section id absent from section_plan");
                continue;
            };
            let view = project_section(section);
            let label = heading_label(&view);
            let slot = &mut self.slots[ix];
            if let Some(label) = label {
                slot.label = label;
                slot.label_refined = true;
            }
            slot.body = Some(view);
            slot.arrived_at = Some(now);
            // Re-measure this one item. Unlike `splice`, this preserves the
            // logical scroll anchor and pins an absolute offset when the
            // anchored item is the one that changed (`list.rs:374`).
            self.list.remeasure_items(ix..ix + 1);
            changed = true;
        }

        self.consumed = sections.len();
        changed
    }

    /// Ingest highlight upgrades for code sections and arm their sweeps.
    ///
    /// Colour only: no span can change a block's height, because that height is
    /// `line_count × mono_line` and mono metrics do not vary with colour
    /// (§9.4.2). We therefore deliberately do *not* remeasure here.
    pub fn sync_highlights(
        &mut self,
        highlights: &std::collections::HashMap<SectionId, Arc<[HighlightSpan]>>,
        cx: &App,
    ) -> bool {
        if highlights.is_empty() {
            return false;
        }
        let (colours, syntax, reduced) = {
            let ext = cx.theme_ext();
            (ext.colours, ext.syntax, ext.reduced_motion())
        };
        let mut changed = false;

        for (id, spans) in highlights {
            if self.highlighted.contains(id) {
                continue;
            }
            let Some(slot) = self.slots.iter_mut().find(|s| s.id == *id) else {
                continue;
            };
            let applied = match slot.body.as_mut() {
                Some(SectionView::Code(code)) => {
                    code.apply_spans(spans, &colours, &syntax, reduced);
                    true
                }
                Some(SectionView::Blocks(blocks)) | Some(SectionView::Callout { blocks, .. }) => {
                    // A `Highlight` for a prose section targets its fenced code;
                    // the protocol carries one span set per section, so it
                    // applies to that section's first code block.
                    match blocks.iter_mut().find_map(|b| match b {
                        BlockView::Code(c) => Some(c),
                        _ => None,
                    }) {
                        Some(code) => {
                            code.apply_spans(spans, &colours, &syntax, reduced);
                            true
                        }
                        None => false,
                    }
                }
                _ => false,
            };
            if applied {
                self.highlighted.push(*id);
                changed = true;
            }
        }
        changed
    }

    /// Advance every running `highlight.sweep` (§4.2 render-loop contract).
    ///
    /// Returns `true` while any sweep is still moving; the caller then asks for
    /// another frame. A fully-settled document ticks nothing at all.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut moving = false;
        for slot in &mut self.slots {
            match slot.body.as_mut() {
                Some(SectionView::Code(code)) => moving |= code.tick(now),
                Some(SectionView::Blocks(blocks)) | Some(SectionView::Callout { blocks, .. }) => {
                    for block in blocks.iter_mut() {
                        if let BlockView::Code(code) = block {
                            moving |= code.tick(now);
                        }
                    }
                }
                _ => {}
            }
        }
        moving
    }

    /// The `SectionId`s currently on screen, for `HighlightPriority` (§9.4.5).
    pub fn visible_ids(&self, range: Range<usize>) -> Vec<SectionId> {
        self.slots
            .get(range)
            .map(|s| s.iter().map(|slot| slot.id).collect())
            .unwrap_or_default()
    }

    /// Scroll so that planned section `ix` is revealed (outline click).
    pub fn reveal(&self, ix: usize) {
        if ix < self.slots.len() {
            self.list.scroll_to_reveal_item(ix);
        }
    }

    /// The index of the topmost item currently anchored.
    pub fn anchor_ix(&self) -> usize {
        self.list.logical_scroll_top().item_ix
    }

    // ── Render ───────────────────────────────────────────────────────────────

    /// Render planned section `ix`. This is `list()`'s item renderer.
    ///
    /// Every path produces an element inside a `min_h(reserved)` box, so the
    /// promise the skeleton made is kept whether or not content has arrived.
    pub fn render_section(&self, ix: usize, _window: &mut Window, cx: &App) -> AnyElement {
        let _span = crate::perf::scope(crate::perf::Region::DocsSection);
        let Some(slot) = self.slots.get(ix) else {
            return div().into_any_element();
        };

        // Every token used below is `Copy`, so the theme borrow ends here and
        // nothing downstream is constrained by it.
        let (sp, ts, colours, scale) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours, ext.motion_scale)
        };

        let frame = div()
            .id(("doc.section", ix))
            .w_full()
            // §9.4: the reserve is the arrived content's floor as well as the
            // skeleton's height, which is what makes arrival a replacement
            // rather than a reflow.
            //
            // Releasing this floor once the document has finished streaming was
            // tried and reverted on 2026-08-09. It is not what produces the
            // dead band below the fields table in `08-symbol-opened.png`: with
            // the release in place the harness reported `consumed=4 slots=4
            // streaming=false reserved=[56, 104, 51, 80]` — 291 logical px of
            // reserve against a `list()` that still laid out 538 — so the
            // floors were already off at capture and the band was unchanged.
            // Whatever is padding that list, it is not this.
            .min_h(slot.reserved)
            .px(sp.space_4)
            .py(sp.space_3);

        let Some(body) = slot.body.as_ref() else {
            return frame
                .child(self.render_skeleton(slot, cx))
                .into_any_element();
        };

        let content: AnyElement = match body {
            SectionView::Blocks(blocks) => div()
                .v_flex()
                .w_full()
                // Paragraph spacing scales with leading. At the old 20 px
                // chrome leading a 12 px gap read as a paragraph break; at the
                // 24 px reading leading it reads as a slightly loose line, and
                // the blocks run together. `space_4` restores the break.
                .gap(sp.space_4)
                .children(
                    blocks
                        .iter()
                        .enumerate()
                        .map(|(bx, block)| self.render_block(ix, bx, block, cx)),
                )
                .into_any_element(),
            SectionView::Callout { level, blocks } => self.render_callout(ix, *level, blocks, cx),
            SectionView::Code(code) => self.render_code(ix, 0, code, cx),
            SectionView::Rows { title, rows } => self.render_rows(ix, title, rows, cx),
            SectionView::Unknown(tag) => div()
                .flex()
                .flex_row()
                .items_center()
                .gap(sp.space_2)
                .child(Badge::custom(
                    ("doc.section.unknown", ix),
                    tag.clone(),
                    colours.bg_hover,
                    colours.fg_muted,
                ))
                .child(
                    div()
                        .text_size(ts.dense.size)
                        .line_height(ts.dense.line_height)
                        .text_color(colours.fg_faint)
                        .child(SharedString::from(
                            "This section was produced by a newer toolchain.",
                        )),
                )
                .into_any_element(),
        };

        // §9.4.3 / LD-19: the entrance is keyed on (generation, section_id), so
        // it fires once when the section lands and never again — not when the
        // reader scrolls back, and not when they leave the tab and return.
        let animate = slot
            .arrived_at
            .is_some_and(|t| t.elapsed() < ROW_CASCADE_WINDOW)
            && scale > 0.0;

        if animate {
            // Borrow the global when it exists; fall back to a local only when
            // it does not (tests, headless), so the hot path allocates nothing.
            let local;
            let motion: &MotionTokens = match cx.try_global::<MotionTokens>() {
                Some(tokens) => tokens,
                None => {
                    local = MotionTokens::new(scale);
                    &local
                }
            };
            let id: ElementId =
                entrance_id("doc.section.enter", self.generation, slot.id.0 as usize);
            frame
                .child(rise_in(div().w_full().child(content), id, motion))
                .into_any_element()
        } else {
            frame.child(content).into_any_element()
        }
    }

    /// The pre-content skeleton: exactly the geometry the plan promised.
    fn render_skeleton(&self, slot: &SectionSlot, cx: &App) -> AnyElement {
        let sp = cx.theme_ext().space;
        let m = &self.metrics;

        let unit = match slot.kind {
            SectionKind::CodeBlock | SectionKind::Examples => m.mono_line,
            SectionKind::Members | SectionKind::Fields => m.row,
            _ => m.prose_line,
        };
        // Derive the bar count from the same reserve the content will honour,
        // so the skeleton is the content's silhouette rather than a guess.
        let usable = slot.reserved - m.pad_y - m.slack;
        let bars = ((f32::from(usable) / f32::from(unit)).round().max(1.0) as usize)
            .min(MAX_SKELETON_BARS);
        let bar_h = unit - sp.space_1;

        div()
            .v_flex()
            .w_full()
            .gap(sp.space_1)
            .children((0..bars).map(|bx| {
                // A ragged right edge on the last bar reads as text rather than
                // as a progress bar.
                let ragged = bars > 1 && bx + 1 == bars;
                Skeleton::new()
                    .h(bar_h)
                    .when(ragged, |s| s.w_3_4())
                    .when(!ragged, |s| s.w_full())
            }))
            .into_any_element()
    }

    fn render_block(
        &self,
        section_ix: usize,
        block_ix: usize,
        block: &BlockView,
        cx: &App,
    ) -> AnyElement {
        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };

        match block {
            // The paragraph is the primary content of this page, and is now
            // set as such.
            //
            // It used to be `ui` type at `fg_muted` — chrome size, secondary
            // colour — while inline code inside it was `fg_default`. So the
            // code fragments *outranked* the sentences containing them, and
            // body, code and links all sat within two points and one step of
            // each other. That is what "flat" meant: nothing in a paragraph
            // had rank. Body now takes the primary foreground and the reading
            // token; code and links are told apart by their background chip
            // and their hue rather than by being brighter than the prose.
            // NOTE — the measure (`sp.measure`) is deliberately NOT applied
            // here, and this is a finding rather than an oversight.
            //
            // Constraining a paragraph to 640 px is the correct typography and
            // it was implemented, shot, and backed out on the evidence.
            // §9.4's zero-jump guarantee reserves each section's height ahead
            // of arrival as `SizeHint::Lines(n) × prose_line`, and that `n`
            // comes from the engine, which does not know the width the text
            // will be laid out at. At the full column width memchr's
            // paragraphs are one line each and the reservation is exact — the
            // shots measured a uniform 44 device px between them. Adding the
            // measure wrapped them to two and three lines, the reservations
            // then under- and over-shot by different amounts per section, and
            // the *rhythm* went to 39 / 25 / 15 logical px — visibly worse
            // than the flat-but-even page it replaced.
            //
            // The measure cannot land until `reserved_height` is computed from
            // the width the text is actually laid out at rather than from a
            // count supplied before layout. That is a §9.4 change, not a view
            // change, and doing it here by widening the slack would trade a
            // visible defect for an invisible one.
            BlockView::Paragraph(rich) => div()
                .w_full()
                .text_size(ts.prose.size)
                .line_height(ts.prose.line_height)
                .text_color(colours.fg_default)
                .child(self.render_rich(section_ix, block_ix, rich, cx))
                .into_any_element(),

            BlockView::Heading { level, text } => {
                // A top-level heading is the one place `display` earns its
                // 20 px: it has to out-rank body text that is now itself
                // 15 px. Sub-headings drop to `title`, which shares the body
                // size and separates on weight alone — the same voice louder,
                // per the `TypeScale` note.
                let scale = if *level <= 1 { ts.display } else { ts.title };
                div()
                    .w_full()
                    .pt(sp.space_2)
                    .text_size(scale.size)
                    .line_height(scale.line_height)
                    .font_weight(gpui::FontWeight(scale.weight as f32))
                    .text_color(colours.fg_default)
                    .child(self.render_rich(section_ix, block_ix, text, cx))
                    .into_any_element()
            }

            // No measure here either, for the reason given on `Paragraph`.
            BlockView::List { items } => div()
                .v_flex()
                .w_full()
                .gap(sp.space_1)
                .pl(sp.space_3)
                .children(items.iter().enumerate().map(|(item_ix, (marker, rich))| {
                    div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(sp.space_2)
                        // Same reading type as a paragraph: a bulleted list is
                        // prose that happens to be enumerated, and setting it
                        // one step smaller was a second, unstated type scale.
                        .text_size(ts.prose.size)
                        .line_height(ts.prose.line_height)
                        .text_color(colours.fg_default)
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(colours.fg_faint)
                                .child(marker.clone()),
                        )
                        .child(div().flex_1().overflow_hidden().child(self.render_rich(
                            section_ix,
                            // Keep list items out of sibling blocks' element-id
                            // space (LD-19).
                            block_ix * 1_000 + item_ix + 1,
                            rich,
                            cx,
                        )))
                }))
                .into_any_element(),

            BlockView::Rule => div()
                .w_full()
                .h(sp.border_width)
                .my(sp.space_2)
                .bg(colours.border_default)
                .into_any_element(),

            BlockView::Code(code) => self.render_code(section_ix, block_ix, code, cx),

            BlockView::Unknown(tag) => Badge::custom(
                ("doc.block.unknown", section_ix * 1_000 + block_ix),
                tag.clone(),
                colours.bg_hover,
                colours.fg_muted,
            )
            .into_any_element(),
        }
    }

    /// One paragraph as a single shaped `StyledText` with live link ranges.
    fn render_rich(
        &self,
        section_ix: usize,
        block_ix: usize,
        rich: &RichText,
        cx: &App,
    ) -> AnyElement {
        let (border_width, colours) = {
            let ext = cx.theme_ext();
            (ext.space.border_width, ext.colours)
        };

        let styles: Vec<(Range<usize>, HighlightStyle)> = rich
            .styles
            .iter()
            .map(|(range, style)| {
                (
                    range.clone(),
                    run_highlight_style(*style, &colours, border_width),
                )
            })
            .collect();

        let text = StyledText::new(rich.text.clone()).with_highlights(styles);
        let id: ElementId = ("doc.rich", section_ix * 100_000 + block_ix).into();

        if rich.link_ranges.is_empty() {
            return InteractiveText::new(id, text).into_any_element();
        }

        let links = rich.links.clone();
        let on_open = self.on_open.clone();
        // The reader's way of asking "what did you change?". The string is the
        // engine's `LinkRepair::note` verbatim — there is no `format!` here,
        // which is what the LR-3 lint enforces and what keeps the explanation
        // authored in one place.
        let notes = rich.repair_notes.clone();
        InteractiveText::new(id, text)
            .on_click(
                rich.link_ranges.to_vec(),
                move |link_ix, window, cx| match links.get(link_ix) {
                    Some(LinkDest::Symbol(key)) => {
                        if let Some(open) = on_open.as_ref() {
                            open(key, window, cx);
                        }
                    }
                    Some(LinkDest::Url(url)) => {
                        if !url.is_empty() {
                            cx.open_url(url);
                        }
                    }
                    None => {}
                },
            )
            .tooltip(move |char_ix, window, cx| {
                notes
                    .iter()
                    .find(|(range, _)| range.contains(&char_ix))
                    .map(|(_, note)| Tooltip::new(note.clone()).build(window, cx))
            })
            .into_any_element()
    }

    /// A fenced code block: fixed height, colour-only upgrades.
    fn render_code(
        &self,
        section_ix: usize,
        block_ix: usize,
        code: &CodeView,
        cx: &App,
    ) -> AnyElement {
        let (sp, ts, colours, kinds, syntax) = {
            let ext = cx.theme_ext();
            (
                ext.space,
                ext.type_scale,
                ext.colours,
                ext.kind_colours,
                ext.syntax,
            )
        };

        let styles: Vec<(Range<usize>, HighlightStyle)> = code
            .spans
            .iter()
            .map(|(range, class_ix)| {
                (
                    range.clone(),
                    HighlightStyle {
                        color: Some(code.class_colour_now(*class_ix, &syntax)),
                        ..Default::default()
                    },
                )
            })
            .collect();

        let text = StyledText::new(code.text.clone()).with_highlights(styles);
        let uid = section_ix * 100_000 + block_ix;

        div()
            .w_full()
            // §9.4.2: the block's height is `line_count` mono lines — fixed
            // before any highlight arrives, and unchanged by every one after.
            .min_h(code.height(&self.metrics))
            .rounded(sp.r_md)
            .bg(colours.bg_base)
            .border_1()
            .border_color(colours.border_default)
            .child(
                div()
                    .id(("doc.code", uid))
                    .w_full()
                    .px(sp.space_3)
                    .py(sp.space_2)
                    .font_family("monospace")
                    .text_size(ts.mono.size)
                    .line_height(ts.mono.line_height)
                    .text_color(colours.fg_muted)
                    // Long lines scroll; they never wrap, so they can never add
                    // a line and never change this block's height.
                    .whitespace_nowrap()
                    .overflow_x_scroll()
                    .child(text),
            )
            .when(!code.lang.is_empty(), |el| {
                el.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .px(sp.space_2)
                        .text_size(ts.caption.size)
                        .line_height(ts.caption.line_height)
                        .text_color(colours.fg_faint)
                        .child(code.lang.clone()),
                )
            })
            .into_any_element()
    }

    fn render_callout(
        &self,
        section_ix: usize,
        level: CalloutLevel,
        blocks: &[BlockView],
        cx: &App,
    ) -> AnyElement {
        // The transparency ladder. Same in every theme — it says how much of a
        // thing is present, not what colour the thing is.
        let al = crate::theme::tokens::AlphaTokens::STANDARD;
        let (sp, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.colours)
        };

        let accent = match level {
            CalloutLevel::Note => colours.info,
            CalloutLevel::Warning => colours.warn,
            CalloutLevel::Danger => colours.danger,
            CalloutLevel::Tip => colours.ok,
            // LD-7: an unknown callout level is neutral, not invisible.
            _ => colours.fg_muted,
        };

        div()
            .v_flex()
            .w_full()
            .gap(sp.space_2)
            .pl(sp.space_3)
            .py(sp.space_2)
            .rounded(sp.r_md)
            // `al.hairline`. `accent` here is the callout's level colour
            // (info / warn / danger / ok), so there is no single role to reach
            // for — this is the case the ladder exists for: the same colour,
            // quieter, by a named amount rather than an invented one.
            .bg(accent.opacity(al.hairline))
            .border_l_2()
            .border_color(accent)
            .children(
                blocks
                    .iter()
                    .enumerate()
                    .map(|(bx, block)| self.render_block(section_ix, bx, block, cx)),
            )
            .into_any_element()
    }

    /// A dense member/field table — the docs.rs pattern, with linked signatures.
    ///
    /// LD-6 note: this is a *section* of the document body, which the enclosing
    /// `list()` already virtualizes. Row counts inside one section are bounded
    /// by the plan's `Rows(n)` hint; a module with thousands of items is chunked
    /// by the producer into several `Members` sections.
    fn render_rows(
        &self,
        section_ix: usize,
        title: &SharedString,
        rows: &[RowView],
        cx: &App,
    ) -> AnyElement {
        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };

        // The table's own heading.
        //
        // # Why this is a landmark and not a caption
        //
        // It used to be `caption` (11 px, muted) over a hairline — the visual
        // weight of a column label. But a `Fields` or `Members` table is a
        // *section of the page*: it is what the outline rail points at, it is
        // where a reader scanning for "what does this type hold" stops, and
        // `08-symbol-opened.png` showed it reading fainter than the body prose
        // above it and fainter than the disclosure headers below it. The page
        // had no rank between "paragraph" and "page section", so everything
        // read at one level (F4).
        //
        // `title` gives it that rank — the same token the disclosure headers
        // use, because they are the same kind of thing. The leading swatch is
        // the kind hue this table's rows are badged with and the one the
        // outline draws beside its row, so a reader can follow one colour from
        // the rail to the heading to the badges.
        let swatch = cx.theme_ext().kind_colours.field;
        div()
            .v_flex()
            .w_full()
            .gap(sp.space_1)
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(sp.space_2)
                    .pb(sp.space_2)
                    .border_b_1()
                    .border_color(colours.border_default)
                    .child(
                        div()
                            .w(sp.border_width * 3.0)
                            .h(ts.title.size)
                            .flex_shrink_0()
                            .rounded(sp.border_width)
                            .bg(swatch),
                    )
                    .child(
                        div()
                            .text_size(ts.title.size)
                            .line_height(ts.title.line_height)
                            .font_weight(gpui::FontWeight(ts.title.weight as f32))
                            .text_color(colours.fg_default)
                            .child(title.clone()),
                    )
                    // The count, in the same relation to its heading as the
                    // disclosure headers' counts are to theirs. Formatted per
                    // render, but from a `usize` on a path that runs once per
                    // arrived section rather than once per row.
                    .child(
                        div()
                            .text_size(ts.caption.size)
                            .line_height(ts.title.line_height)
                            .text_color(colours.fg_faint)
                            .child(SharedString::from(rows.len().to_string())),
                    ),
            )
            .children(rows.iter().enumerate().map(|(rx, row)| {
                let row_id = section_ix * 100_000 + rx;
                let key = row.key.clone();
                let on_open = self.on_open.clone();
                let links = row.links.clone();
                let link_open = self.on_open.clone();
                let source_handler = self.on_source.clone();
                let source_target = row.source.clone();

                let badge: AnyElement = match &row.kind {
                    KindChip::Known(k) => {
                        Badge::for_kind(("doc.row.kind", row_id), *k, cx).into_any_element()
                    }
                    KindChip::Unknown(label) => Badge::custom(
                        ("doc.row.kind", row_id),
                        label.clone(),
                        colours.bg_hover,
                        colours.fg_muted,
                    )
                    .into_any_element(),
                };

                div()
                    .id(("doc.row", row_id))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(sp.space_2)
                    .px(sp.space_2)
                    .py(sp.space_1)
                    .rounded(sp.r_sm)
                    .cursor_pointer()
                    .hover(|s| s.bg(colours.bg_hover))
                    .active(|s| s.bg(colours.bg_active))
                    .when_some(on_open, |el, open| {
                        el.on_click(move |_, window, cx| open(&key, window, cx))
                    })
                    .child(div().flex_shrink_0().child(badge))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(ts.dense.size)
                            .line_height(ts.dense.line_height)
                            .font_weight(gpui::FontWeight(ts.caption.weight as f32))
                            .text_color(colours.fg_default)
                            .child(row.name.clone()),
                    )
                    .child(div().flex_1().overflow_hidden().child(
                        // LR-4: the *only* signature renderer, here as
                        // everywhere else. Types stay clickable inside rows.
                        SignatureLine::new(("doc.row.sig", row_id), row.sig.clone()).on_navigate(
                            move |ui_key, window, cx| {
                                if let Some(target) = link_ix(ui_key).and_then(|ix| links.get(ix)) {
                                    if let Some(open) = link_open.as_ref() {
                                        open(target, window, cx);
                                    }
                                }
                            },
                        ),
                    ))
                    .when_some(source_handler, |el, handler| {
                        let Some(target) = source_target.clone() else {
                            return el;
                        };
                        let display_target = target.clone();
                        el.child(
                            div()
                                .id(("doc.row.source", row_id))
                                .flex_shrink_0()
                                .font_family("monospace")
                                .text_size(ts.caption.size)
                                .line_height(ts.dense.line_height)
                                .text_color(colours.accent)
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .on_click(move |_, window, cx| {
                                    handler(&target, window, cx);
                                })
                                .child(display_target),
                        )
                    })
            }))
            .into_any_element()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Free projection helpers
// ─────────────────────────────────────────────────────────────────────────────

fn project_section(section: &RenderSection) -> SectionView {
    match section {
        RenderSection::Prose { blocks, .. } | RenderSection::Examples { blocks, .. } => {
            SectionView::Blocks(blocks.iter().map(project_block).collect())
        }
        RenderSection::Callout { level, blocks, .. } => SectionView::Callout {
            level: *level,
            blocks: blocks.iter().map(project_block).collect(),
        },
        RenderSection::CodeBlock {
            lang,
            text,
            line_count,
            ..
        } => SectionView::Code(CodeView::new(shared(text), shared(&lang.0), *line_count)),
        RenderSection::Members { entries, .. } => SectionView::Rows {
            title: SharedString::from("Members"),
            rows: entries.iter().map(RowView::from_member).collect(),
        },
        RenderSection::Fields { entries, .. } => SectionView::Rows {
            title: SharedString::from("Fields"),
            rows: entries.iter().map(RowView::from_field).collect(),
        },
        RenderSection::Unknown { kind_tag, .. } => SectionView::Unknown(shared(kind_tag)),
        // LD-7: a section kind added after this binary was built.
        _ => SectionView::Unknown(SharedString::from("unknown section")),
    }
}

fn project_block(block: &ProseBlock) -> BlockView {
    match block {
        ProseBlock::Paragraph { runs } => BlockView::Paragraph(RichText::from_runs(runs)),
        ProseBlock::Heading { level, runs } => BlockView::Heading {
            level: *level,
            text: RichText::from_runs(runs),
        },
        ProseBlock::List { ordered, items } => BlockView::List {
            items: items
                .iter()
                .enumerate()
                .map(|(ix, runs)| {
                    // Markers are built here, not in render (§1.1.4).
                    let marker = if *ordered {
                        SharedString::from(format!("{}.", ix + 1))
                    } else {
                        SharedString::from("•")
                    };
                    (marker, RichText::from_runs(runs))
                })
                .collect(),
        },
        ProseBlock::Rule => BlockView::Rule,
        ProseBlock::Code {
            lang,
            text,
            line_count,
        } => BlockView::Code(CodeView::new(shared(text), shared(&lang.0), *line_count)),
        // LD-7 fallback.
        _ => BlockView::Unknown(SharedString::from("unknown block")),
    }
}

/// The outline label a section earns once its content arrives.
///
/// A heading beats a kind name, and a `Members`/`Fields` table names itself.
/// Everything else keeps the plan's label.
fn heading_label(view: &SectionView) -> Option<SharedString> {
    match view {
        SectionView::Blocks(blocks) | SectionView::Callout { blocks, .. } => {
            blocks.iter().find_map(|b| match b {
                BlockView::Heading { text, .. } if !text.text.is_empty() => Some(text.text.clone()),
                _ => None,
            })
        }
        SectionView::Rows { title, .. } => Some(title.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::px;

    fn metrics() -> Metrics {
        Metrics {
            prose_line: px(20.0),
            mono_line: px(19.0),
            row: px(24.0),
            pad_y: px(24.0),
            slack: px(8.0),
        }
    }

    fn palette() -> (ColourRoles, KindColours) {
        let theme = crate::theme::default_theme();
        (theme.colours, theme.kind_colours)
    }

    fn palette_syntax() -> SyntaxColours {
        crate::theme::default_theme().syntax
    }

    /// §9.4.1: `Lines(n)` reserves `n` line heights, plus the section's own
    /// padding and rise slack.
    #[test]
    fn lines_hint_reserves_n_prose_lines() {
        let m = metrics();
        let h = reserved_height(SectionKind::Prose, SizeHint::Lines(5), &m);
        assert_eq!(h, px(20.0) * 5.0 + px(24.0) + px(8.0));
    }

    /// Code and example sections measure in mono lines, not prose lines —
    /// otherwise a 40-line sample reserves the wrong height.
    #[test]
    fn code_hint_reserves_mono_lines() {
        let m = metrics();
        let h = reserved_height(SectionKind::CodeBlock, SizeHint::Lines(4), &m);
        assert_eq!(h, px(19.0) * 4.0 + px(24.0) + px(8.0));
    }

    /// §9.4.1: `Rows(n)` reserves `n` row heights.
    #[test]
    fn rows_hint_reserves_n_rows() {
        let m = metrics();
        let h = reserved_height(SectionKind::Members, SizeHint::Rows(10), &m);
        assert_eq!(h, px(24.0) * 10.0 + px(24.0) + px(8.0));
    }

    /// §9.4.1: no estimate is a three-line block, never zero. A zero-height
    /// skeleton collapses the list and hands the reader a jump on arrival.
    #[test]
    fn unknown_hint_reserves_three_lines() {
        let m = metrics();
        let h = reserved_height(SectionKind::Prose, SizeHint::Unknown, &m);
        assert_eq!(h, px(20.0) * 3.0 + px(24.0) + px(8.0));
    }

    /// A degenerate `Lines(0)` must still reserve one line.
    #[test]
    fn zero_lines_still_reserves_one() {
        let m = metrics();
        let h = reserved_height(SectionKind::Prose, SizeHint::Lines(0), &m);
        assert_eq!(h, px(20.0) + px(24.0) + px(8.0));
    }

    /// The rise slack must exceed `rise_in`'s 6 px offset, or the entrance
    /// animation pushes a section past its reserve and forces a re-measure on
    /// every frame of the animation.
    #[test]
    fn slack_absorbs_the_rise_offset() {
        assert!(metrics().slack >= px(6.0));
    }

    /// Inline runs flatten to one string with contiguous, non-overlapping,
    /// in-order style ranges — the invariant `StyledText` relies on.
    #[test]
    fn rich_text_ranges_are_contiguous_and_ordered() {
        let runs = vec![
            InlineRun::Text {
                text: "see ".into(),
            },
            InlineRun::Code { text: "Vec".into() },
            InlineRun::Text {
                text: " and ".into(),
            },
            InlineRun::Link {
                text: "HashMap".into(),
                target: LinkTarget::Url {
                    url: "https://example.invalid".into(),
                },
                origin: LinkOrigin::Authored,
            },
        ];
        let rich = RichText::from_runs(&runs);
        assert_eq!(&*rich.text, "see Vec and HashMap");

        let mut cursor = 0usize;
        for (range, _) in rich.styles.iter() {
            assert_eq!(range.start, cursor, "style ranges must be contiguous");
            assert!(range.end > range.start, "empty style range");
            cursor = range.end;
        }
        assert_eq!(
            cursor,
            rich.text.len(),
            "styles must cover the whole string"
        );
    }

    /// A link's clickable range must address exactly its own text, or clicking
    /// one link opens another.
    #[test]
    fn link_ranges_address_their_own_text() {
        let runs = vec![
            InlineRun::Text {
                text: "go to ".into(),
            },
            InlineRun::Link {
                text: "Result".into(),
                target: LinkTarget::Url {
                    url: "https://example.invalid".into(),
                },
                origin: LinkOrigin::Authored,
            },
            InlineRun::Text {
                text: " now".into(),
            },
        ];
        let rich = RichText::from_runs(&runs);
        assert_eq!(rich.link_ranges.len(), 1);
        let r = rich.link_ranges[0].clone();
        assert_eq!(&rich.text[r], "Result");
        assert_eq!(rich.links.len(), 1);
        assert!(
            rich.repair_notes.is_empty(),
            "an authored link must carry no repair note — a note on every link \
             would make the mark meaningless"
        );
    }

    /// A repaired link must project to its own paint *and* its own note, keyed
    /// to exactly the link's byte range, with the engine's sentence unchanged.
    ///
    /// # What each assertion is for
    ///
    /// * `RunStyle::RepairedLink`, not `RunStyle::Link` — the reader can see
    ///   which links the crate author wrote and which we inferred.
    /// * The note's range equals the link's `link_ranges` entry — the tooltip
    ///   fires over the link and nowhere else. A drifting range would put the
    ///   explanation on the wrong words.
    /// * The note text is **`==` the engine's `note`** — that equality is the
    ///   proof that lindsey did no string work (LR-3). A `format!` here would
    ///   put half the explanation in the view layer where nothing tests it.
    #[test]
    fn repaired_link_projects_to_its_own_run_style_and_note() {
        let note = "Repaired link — the source reads `[memrchr_iter`] \
                    (transposed backtick and bracket); we linked memrchr_iter.";
        let runs = vec![
            InlineRun::Text {
                text: "see ".into(),
            },
            InlineRun::Link {
                text: "memrchr_iter".into(),
                target: LinkTarget::Url {
                    url: "https://example.invalid".into(),
                },
                origin: LinkOrigin::Repaired(nudox_engine::wire::LinkRepair {
                    kind: LinkRepairKind::TransposedOpenDelimiter,
                    raw: "`[memrchr_iter`]".into(),
                    resolved: "memrchr_iter".into(),
                    note: note.into(),
                }),
            },
        ];
        let rich = RichText::from_runs(&runs);

        let link_style = rich
            .styles
            .iter()
            .find(|(_, s)| matches!(s, RunStyle::RepairedLink | RunStyle::Link))
            .expect("the link run must contribute a style range");
        assert_eq!(
            link_style.1,
            RunStyle::RepairedLink,
            "a repaired link must not paint as an ordinary link"
        );

        assert_eq!(rich.link_ranges.len(), 1);
        assert_eq!(rich.repair_notes.len(), 1);
        assert_eq!(
            rich.repair_notes[0].0, rich.link_ranges[0],
            "the note must cover exactly the link's own text"
        );
        assert_eq!(&rich.text[rich.repair_notes[0].0.clone()], "memrchr_iter");
        assert_eq!(
            rich.repair_notes[0].1.as_ref(),
            note,
            "the note must be the engine's sentence verbatim — any difference \
             means lindsey did string work the chunker owns (LR-3)"
        );
    }

    /// The legend must name every repair kind. `LinkRepairKind` is exhaustive,
    /// so `repair_legend_label` does not compile until a new repair has a
    /// reader-facing name — this test additionally proves the names are real
    /// and distinct rather than a placeholder repeated.
    #[test]
    fn every_repair_kind_has_a_distinct_reader_facing_label() {
        let mut seen: Vec<SharedString> = Vec::new();
        for &kind in LinkRepairKind::ALL {
            let label = repair_legend_label(kind);
            assert!(!label.is_empty(), "{kind:?} has an empty legend label");
            assert!(
                !seen.contains(&label),
                "two repair kinds share the legend label {label:?}"
            );
            seen.push(label);
        }
        assert_eq!(seen.len(), LinkRepairKind::ALL.len());
    }

    /// An empty run contributes no style range — an empty `(0..0, Text)` pair
    /// would trip `StyledText`'s run arithmetic.
    #[test]
    fn empty_runs_contribute_no_range() {
        let rich = RichText::from_runs(&[InlineRun::Text { text: "".into() }]);
        assert!(rich.styles.is_empty());
        assert!(rich.text.is_empty());
    }

    /// A code block's height depends only on `line_count`, never on whether
    /// highlights have arrived. This is §9.4.2 in one assertion.
    #[test]
    fn highlight_cannot_change_code_height() {
        let m = metrics();
        let (colours, _kinds) = palette();
        let syntax = palette_syntax();
        let mut code = CodeView::new("fn main() {}".into(), "rust".into(), 3);
        let before = code.height(&m);

        code.apply_spans(
            &[HighlightSpan {
                start: 0,
                end: 2,
                class: "keyword".into(),
            }],
            &colours,
            &syntax,
            true,
        );

        assert_eq!(code.height(&m), before, "colour must not move geometry");
        assert_eq!(code.spans.len(), 1);
    }

    /// Out-of-range, inverted and empty spans are dropped, not panicked on: a
    /// producer bug must never take the page down (LD-7).
    #[test]
    fn malformed_spans_are_dropped() {
        let (colours, _kinds) = palette();
        let syntax = palette_syntax();
        let mut code = CodeView::new("ab".into(), "rust".into(), 1);
        code.apply_spans(
            &[
                HighlightSpan {
                    start: 0,
                    end: 99,
                    class: "keyword".into(),
                },
                HighlightSpan {
                    start: 5,
                    end: 1,
                    class: "string".into(),
                },
                HighlightSpan {
                    start: 1,
                    end: 1,
                    class: "string".into(),
                },
                HighlightSpan {
                    start: 0,
                    end: 1,
                    class: "keyword".into(),
                },
            ],
            &colours,
            &syntax,
            true,
        );
        assert_eq!(code.spans.len(), 1, "only the valid span survives");
    }

    /// Spans that would split a multi-byte character are dropped rather than
    /// reaching `StyledText`'s char-boundary debug assertion.
    #[test]
    fn non_boundary_spans_are_dropped() {
        let (colours, _kinds) = palette();
        let syntax = palette_syntax();
        let mut code = CodeView::new("é".into(), "rust".into(), 1);
        code.apply_spans(
            &[HighlightSpan {
                start: 0,
                end: 1,
                class: "keyword".into(),
            }],
            &colours,
            &syntax,
            true,
        );
        assert!(code.spans.is_empty());
    }

    /// Reduced motion snaps the sweep: the colour still changes, the time
    /// dimension does not exist (LD-17).
    #[test]
    fn reduced_motion_snaps_the_sweep() {
        let (colours, _kinds) = palette();
        let syntax = palette_syntax();
        let mut code = CodeView::new("let x = 1;".into(), "rust".into(), 1);
        code.apply_spans(
            &[HighlightSpan {
                start: 0,
                end: 3,
                class: "keyword".into(),
            }],
            &colours,
            &syntax,
            true,
        );
        assert!(
            !code.tick(Instant::now()),
            "a snapped sweep must not animate"
        );
    }

    /// Distinct classes get distinct springs; repeats share one.
    #[test]
    fn classes_are_deduplicated() {
        let (colours, _kinds) = palette();
        let syntax = palette_syntax();
        let mut code = CodeView::new("let x = 1;".into(), "rust".into(), 1);
        code.apply_spans(
            &[
                HighlightSpan {
                    start: 0,
                    end: 3,
                    class: "keyword".into(),
                },
                HighlightSpan {
                    start: 4,
                    end: 5,
                    class: "variable".into(),
                },
                HighlightSpan {
                    start: 8,
                    end: 9,
                    class: "keyword".into(),
                },
            ],
            &colours,
            &syntax,
            true,
        );
        assert_eq!(code.classes.len(), 2);
        assert_eq!(code.spans.len(), 3);
    }

    /// An unknown token class still paints readable text rather than vanishing.
    #[test]
    fn unknown_class_is_readable() {
        let syntax = palette_syntax();
        assert_eq!(class_colour("no-such-class", &syntax), syntax.ident);
    }

    /// Ordered list markers are built at projection time, so `render` never
    /// formats a string (§1.1.4).
    #[test]
    fn ordered_list_markers_are_precomputed() {
        let block = ProseBlock::List {
            ordered: true,
            items: vec![
                vec![InlineRun::Text { text: "one".into() }],
                vec![InlineRun::Text { text: "two".into() }],
            ],
        };
        match project_block(&block) {
            BlockView::List { items } => {
                assert_eq!(&*items[0].0, "1.");
                assert_eq!(&*items[1].0, "2.");
            }
            _ => panic!("expected a list block"),
        }
    }

    /// Every planned section kind gets a non-empty outline label *before* any
    /// content exists — the thing docs.rs structurally cannot do.
    #[test]
    fn every_planned_kind_has_a_label() {
        for kind in [
            SectionKind::Prose,
            SectionKind::CodeBlock,
            SectionKind::Members,
            SectionKind::Fields,
            SectionKind::Examples,
            SectionKind::Callout,
            SectionKind::Unknown,
        ] {
            assert!(
                !kind_label(kind).is_empty(),
                "{kind:?} has no outline label"
            );
        }
    }

    /// Declared member locations must survive projection as a concrete,
    /// package-relative copy target; displaying only the member name leaves
    /// the source fact unreachable to the reader.
    #[test]
    fn declared_member_source_projects_to_a_copy_target() {
        use std::num::NonZeroU32;

        use nudox_engine::wire::{EcosystemId, IntroId, PackageLineageId, PackageName};

        let lineage = PackageLineageId::new(EcosystemId::new("test"), PackageName::new("pkg"));
        let key = SymbolKey::new(lineage, IntroId::from_raw([7_u8; 32]));
        let row = MemberRow {
            key,
            name: "route".into(),
            sig: Vec::new(),
            kind: nudox_engine::wire::KindTag::Unknown(1),
            visibility: nudox_engine::wire::Visibility::Public,
            source: nudox_engine::wire::SourceLocation::Declared {
                file: "src/router.rs".into(),
                bytes: [10, 20],
                start: nudox_engine::wire::LineCol {
                    line: NonZeroU32::new(4).unwrap(),
                    column: NonZeroU32::new(1).unwrap(),
                },
                end: nudox_engine::wire::LineCol {
                    line: NonZeroU32::new(4).unwrap(),
                    column: NonZeroU32::new(6).unwrap(),
                },
            },
        };

        let view = RowView::from_member(&row);
        assert_eq!(view.source.as_deref(), Some("src/router.rs:4:1"));
    }

    /// A heading refines the outline label; a section without one keeps the
    /// plan's placeholder.
    #[test]
    fn headings_refine_the_outline_label() {
        let with_heading = project_section(&RenderSection::Prose {
            id: SectionId(1),
            blocks: vec![ProseBlock::Heading {
                level: 1,
                runs: vec![InlineRun::Text {
                    text: "Errors".into(),
                }],
            }],
        });
        assert_eq!(heading_label(&with_heading).as_deref(), Some("Errors"));

        let without = project_section(&RenderSection::Prose {
            id: SectionId(2),
            blocks: vec![ProseBlock::Paragraph {
                runs: vec![InlineRun::Text { text: "hi".into() }],
            }],
        });
        assert!(heading_label(&without).is_none());
    }

    /// An unknown section renders as a chip carrying the producer's own tag,
    /// never as an empty gap (LD-7).
    #[test]
    fn unknown_section_keeps_its_tag() {
        let view = project_section(&RenderSection::Unknown {
            id: SectionId(3),
            kind_tag: "diagram".into(),
        });
        match view {
            SectionView::Unknown(tag) => assert_eq!(&*tag, "diagram"),
            _ => panic!("expected an unknown section"),
        }
    }
}
