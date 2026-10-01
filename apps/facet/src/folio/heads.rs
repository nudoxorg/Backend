//! The heads-up stack: what a package does to your build and your machine,
//! read from its own source. At rest a hand of small chips lies compact,
//! one over the next; rest on the cell and the hand fans out, each chip
//! taking its words and its count; click and every finding is laid out on
//! one sheet with the lines of source that show it.
//!
//! Findings speak plainly and in order of consequence: a build script or a
//! procedural macro (it runs code on your machine while you compile), a
//! program it starts, a call into C, then what it names of the network, the
//! files and the environment, then what it says about `unsafe`. A package
//! with nothing to flag says so, in mint.

use super::state::{Build, DisclosureCopy, DisclosureFlow, InkPhase, Library, Nominal, Pose, Unsafe};
use super::text::{ellipsis, key, natural_width, one, wrap};
use crate::controls::button::wire;
use crate::controls::state::{Touch, hover_zone};
use crate::marks::badges::{Glyph, glyph};
use crate::measure::{Measure, Space};
use crate::overlay::dialog::{self, Dialog, DialogButton};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::probe;
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    AnyElement, App, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce, SharedString, Styled, Window,
    div, px,
};
use std::rc::Rc;

/// How loudly a finding speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Tone {
    /// Runs code on your machine, or reaches outside the language.
    Warn,
    /// Worth knowing.
    Note,
    /// The safe side.
    Good,
}

/// One line of source that shows a finding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Place {
    /// The file, relative to the crate.
    pub file: SharedString,
    /// The line, from one.
    pub line: usize,
    /// The line itself.
    pub text: SharedString,
}

/// One thing a package does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Finding {
    /// Its glyph.
    pub glyph: Glyph,
    /// How loudly it speaks.
    pub tone: Tone,
    /// What it does, in words.
    pub word: SharedString,
    /// Why it matters, in a sentence.
    pub why: SharedString,
    /// In how many places, when it is counted.
    pub count: Option<usize>,
    /// Lines that show it.
    pub places: Vec<Place>,
}

/// One kind of thing a scan found: how many lines, and the first few.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Sighting {
    /// How many lines name it.
    pub count: usize,
    /// The first of them, with file and line.
    pub places: Vec<Place>,
}

/// What a scan of a package's source found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Signals {
    /// Whether building it runs its own code.
    pub build: Build,
    /// Whether it is a procedural macro.
    pub library: Library,
    /// Whether its crate root forbids `unsafe`.
    pub unsafe_code: Unsafe,
    /// `unsafe` blocks, functions and impls.
    pub unsafe_count: usize,
    /// Programs it starts.
    pub process: Sighting,
    /// Calls into C.
    pub ffi: Sighting,
    /// The network.
    pub net: Sighting,
    /// Files.
    pub files: Sighting,
    /// The environment.
    pub env: Sighting,
}

fn finding(glyph: Glyph, tone: Tone, word: &str, why: &str, count: Option<usize>, places: &[Place]) -> Finding {
    Finding { glyph, tone, word: word.to_owned().into(), why: why.to_owned().into(), count, places: places.to_vec() }
}

/// Every finding, most consequential first.
#[must_use]
pub fn findings(signals: &Signals) -> Vec<Finding> {
    let mut out = Vec::new();
    if signals.build == Build::Script {
        out.push(finding(Glyph::Build, Tone::Warn, "Runs code when you build", "It has a build script (build.rs): code that runs on your machine at compile time.", None, &[]));
    }
    if signals.library == Library::ProcMacro {
        out.push(finding(Glyph::MacroPkg, Tone::Warn, "Runs inside your compiler", "It is a procedural macro: its code runs in rustc while your code compiles.", None, &[]));
    }
    if signals.process.count > 0 {
        out.push(finding(Glyph::Process, Tone::Warn, "Starts programs", "It spawns other programs.", Some(signals.process.count), &signals.process.places));
    }
    if signals.ffi.count > 0 {
        out.push(finding(Glyph::Ffi, Tone::Warn, "Calls C", "It crosses into C code, where Rust's guarantees stop.", Some(signals.ffi.count), &signals.ffi.places));
    }
    if signals.net.count > 0 {
        out.push(finding(Glyph::Net, Tone::Note, "Opens network connections", "It names sockets or HTTP clients.", Some(signals.net.count), &signals.net.places));
    }
    if signals.files.count > 0 {
        out.push(finding(Glyph::Files, Tone::Note, "Touches files", "It reads or writes the file system.", Some(signals.files.count), &signals.files.places));
    }
    if signals.env.count > 0 {
        out.push(finding(Glyph::Env, Tone::Note, "Reads environment variables", "Its behaviour can change with your environment.", Some(signals.env.count), &signals.env.places));
    }
    if signals.unsafe_code == Unsafe::Forbidden {
        out.push(finding(Glyph::Shield, Tone::Good, "Forbids unsafe code", "#![forbid(unsafe_code)]: the compiler guarantees it has none.", None, &[]));
    } else if signals.unsafe_count > 0 {
        out.push(finding(
            Glyph::Unsafe,
            if signals.unsafe_count > 50 { Tone::Warn } else { Tone::Note },
            "Unsafe blocks",
            "Places where it promises what the compiler can't check.",
            Some(signals.unsafe_count),
            &[],
        ));
    }
    out
}

fn ink_of(tone: Tone, palette: &Palette) -> Hsla {
    match tone {
        Tone::Warn => palette.amber.base.into(),
        Tone::Note => palette.ink1.into(),
        Tone::Good => palette.mint.base.into(),
    }
}

fn edge_of(tone: Tone, palette: &Palette) -> Edge {
    match tone {
        Tone::Warn => Edge::of(Bevel::Amber, palette),
        Tone::Good => Edge::of(Bevel::Hot, palette),
        Tone::Note => {
            let mut edge = Edge::of(Bevel::Rest, palette);
            edge.hi = palette.line3.into();
            edge.lo = palette.line2.into();
            edge
        }
    }
}

const WORD: TypeRole = TypeRole { weight: 600.0, size: 12.0, line: 16.0, ..ty::SMALL };
const COUNT: TypeRole = TypeRole { size: 11.5, line: 16.0, ..ty::MONO_SMALL };
const NOTHING: TypeRole = TypeRole { size: 12.0, line: 16.0, ..ty::SMALL };

/// The chip's height and the stack's overlap, px at 100 %.
const CHIP: f32 = 28.0;
/// How far a chip lies over the one before it at rest: little enough that each
/// chip's glyph (7 to 21 px into its 28) shows whole.
const OVERLAP: f32 = 6.0;

/// The heads-up cell (see [`heads`]).
#[derive(IntoElement)]
pub struct HeadsUp {
    id: ElementId,
    findings: Rc<Vec<Finding>>,
    package: SharedString,
    measure: Measure,
    width: Pixels,
    spread: Pixels,
    height: Nominal,
    held: Pose,
}

/// The cell for `findings` of `package`, `width` px at rest and fanning out
/// to at most `spread` px, `height` px tall at 100 %.
#[must_use]
pub fn heads(id: impl Into<ElementId>, package: impl Into<SharedString>, findings: Rc<Vec<Finding>>, width: Pixels, spread: Pixels, height: Nominal, measure: &Measure) -> HeadsUp {
    HeadsUp { id: id.into(), findings, package: package.into(), measure: *measure, width, spread, height, held: Pose::Live }
}

impl HeadsUp {
    /// Shows the hand fanned out whatever the pointer does (scenes, tests).
    #[must_use]
    pub const fn open(mut self) -> Self {
        self.held = Pose::Held;
        self
    }
}

/// Opens the sheet of `findings` (a click on the hand, or Enter on it).
pub fn open_sheet(package: &str, findings: Rc<Vec<Finding>>, window: &mut Window, cx: &mut App) {
    dialog::open(sheet(package, findings), window, cx);
}

/// The sheet every finding opens onto.
#[must_use]
pub fn sheet(package: &str, findings: Rc<Vec<Finding>>) -> Dialog {
    let content: dialog::SheetContent = {
        let package = package.to_owned();
        Rc::new(move |measure, _window, cx| sheet_body(&package, &findings, measure, cx))
    };
    Dialog {
        title: format!("{package}, read from its own source").into(),
        body: SharedString::default(),
        buttons: vec![DialogButton::new("Close", |_, _| {}).primary()],
        dismissible: true,
        sheet: Some(content),
    }
}

fn sheet_body(package: &str, findings: &[Finding], measure: &Measure, cx: &App) -> AnyElement {
    let palette = cx.palette();
    let id: ElementId = ElementId::Name("heads-sheet".into());
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base));
    if findings.is_empty() {
        column = column.child(one(key(&id, "none"), "Nothing it does needs a heads-up.", NOTHING, palette.mint.base, measure));
    }
    for (index, item) in findings.iter().enumerate() {
        let ink = ink_of(item.tone, palette);
        let mut title = div().flex().items_baseline().gap(measure.space(Space::Snug)).child(one(key(&id, format!("w{index}")), item.word.clone(), TypeRole { size: 14.0, line: 20.0, ..WORD }, palette.ink0, measure));
        if let Some(count) = item.count {
            title = title.child(one(key(&id, format!("n{index}")), format!("{count} {}", if count == 1 { "place" } else { "places" }), COUNT, palette.ink3, measure));
        }
        let mut body = div().flex().flex_col().gap(measure.space(Space::Tight)).min_w_0().flex_1().child(title).child(wrap(
            key(&id, format!("why{index}")),
            item.why.clone(),
            TypeRole { size: 13.5, line: 19.0, ..ty::CAPTION },
            palette.ink2,
            measure,
            None,
        ));
        for (n, place) in item.places.iter().enumerate() {
            body = body.child(
                div()
                    .flex()
                    .gap(measure.space(Space::Roomy))
                    .min_w_0()
                    .child(one(key(&id, format!("p{index}-{n}")), format!("{}:{}", place.file, place.line), COUNT, palette.ink3, measure))
                    .child(div().flex_1().min_w_0().flex().child(ellipsis(key(&id, format!("t{index}-{n}")), place.text.clone(), COUNT, palette.ink1, measure))),
            );
        }
        column = column.child(
            div()
                .flex()
                .gap(measure.space(Space::Roomy))
                .pt(measure.space(Space::Roomy))
                .border_t_1()
                .border_color(palette.line1.hsla())
                .child(
                    div()
                        .flex_none()
                        .size(px(30.0 * measure.scale()))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(glyph(item.glyph, 18.0 * measure.scale(), ink)),
                )
                .child(body),
        );
    }
    let _ = package;
    column.into_any_element()
}

impl RenderOnce for HeadsUp {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let scale = measure.scale();
        let mut touch = Touch::read(&self.id, crate::controls::Look::LIVE, true, window, cx);
        // The card stays open while its control owns keyboard focus, even if
        // a resize causes the platform to reconcile the parked pointer and
        // switches the :focus-visible modality back to mouse. The open
        // disclosure is the focus affordance here; tying it to the transient
        // modality would close the card while focus never moved.
        touch.focused = touch.focus.is_focused(window);
        if let Some((_, target)) = touch.claim.as_mut() {
            target.focused = touch.focused;
        }
        let disclosure = DisclosureCopy::read(&self.id, &touch, self.held, window, cx);
        let open = disclosure.geometry;
        let findings = self.findings.clone();
        let warns = findings.iter().filter(|f| f.tone == Tone::Warn).count();
        let chip = CHIP * scale;
        let overlap = OVERLAP * scale;
        let gap = f32::from(measure.space(Space::Snug));
        let pad = f32::from(measure.space(Space::Roomy));
        // Measure each complete chip with the platform text system, then cap
        // it to the space this card can occupy. Labels wrap inside that real
        // width and their measured height participates in ordinary flow.
        let word_role = measure.role(WORD);
        let count_role = measure.role(COUNT);
        let available_width = f32::from(measure.width()).min(f32::from(self.spread));
        let base_width = f32::from(self.width).min(available_width);
        let natural_widths: Vec<f32> = findings
            .iter()
            .map(|f| {
                let word = f32::from(natural_width(&f.word, word_role, window));
                let count = f.count.map_or(0.0, |n| f32::from(natural_width(&SharedString::from(n.to_string()), count_role, window)) + gap);
                (10.0 * scale + 14.0 * scale + gap + word + count + 10.0 * scale).max(chip)
            })
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let gaps = findings.len().saturating_sub(1) as f32;
        let wanted_width = natural_widths.iter().sum::<f32>() + gap * gaps + 2.0 * pad;
        let target_plate_width = wanted_width.max(base_width).min(available_width);
        let target_room = (target_plate_width - 2.0 * pad).max(chip);
        let target_widths = natural_widths.iter().map(|width| width.min(target_room)).collect::<Vec<_>>();
        let plate_width = base_width + (target_plate_width - base_width) * open.min(1.0);
        // One finding alone says its words at rest, when its cell holds them: a
        // lone icon in an otherwise empty tile says nothing.
        let lone = findings.len() == 1
            && natural_widths
                .first()
                .is_some_and(|width| *width <= (base_width - 2.0 * pad).max(0.0));
        let base_h = DisclosureFlow::rest_height(self.height);

        // At rest the chips overlap slightly. As they open they unwrap into
        // bounded flex rows, with GPUI measuring every wrapped label's height.
        let mut hand = div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap_x(px(gap * open.min(1.0)))
            .gap_y(px(4.0 * scale * open.min(1.0)))
            .w_full()
            .min_w_0();
        if findings.is_empty() {
            hand = hand.child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Snug))
                    .min_w_0()
                    .w_full()
                    .min_h(px(chip))
                    .child(glyph(Glyph::Shield, 16.0 * scale, palette.mint.base))
                    .child(div().flex_1().min_w_0().child(wrap(
                        key(&self.id, "nothing"),
                        "Nothing to flag",
                        NOTHING,
                        palette.mint.base,
                        &measure,
                        None,
                    ))),
            );
        }
        for (i, item) in findings.iter().enumerate() {
            let shown = if lone { 1.0 } else { open.min(1.0) };
            let target_width = target_widths[i];
            let width = chip + (target_width - chip) * shown;
            let ink = ink_of(item.tone, palette);
            let icon_inset = (chip - 14.0 * scale) * 0.5;
            let copy_visible = lone || disclosure.ink == InkPhase::Retained;
            let right_inset = if copy_visible { 10.0 * scale } else { 0.0 };
            let row_text_room = width - icon_inset - 14.0 * scale - gap - right_inset;
            let stacked_text_room = width - icon_inset - right_inset;
            // Keep at least four ems for the identifier before spending that
            // width on a non-wrapping count. If the icon itself leaves less
            // room than that, move it above the copy so the label can use the
            // full tile width. Both choices follow the measured tile width;
            // neither clips nor truncates the finding.
            let readable_word_room = word_role.size * 4.0;
            let stack_icon = copy_visible
                && row_text_room < readable_word_room
                && stacked_text_room > row_text_room;
            let label_room = if stack_icon { stacked_text_room } else { row_text_room };
            let count_width = item.count.map_or(0.0, |count| {
                f32::from(natural_width(&SharedString::from(count.to_string()), count_role, window))
            });
            let show_copy = copy_visible && label_room >= word_role.size;
            let count_inline = show_copy
                && !stack_icon
                && item.count.is_some()
                && row_text_room - count_width - gap >= readable_word_room;
            let mut inner = if stack_icon {
                div().flex().flex_col().items_start().min_w_0().gap(px(gap))
            } else {
                div().flex().items_start().min_w_0().gap(px(gap))
            }
            .pl(px(icon_inset))
            .pr(px(if show_copy { right_inset } else { 0.0 }))
            .child(probe::measure(
                key(&self.id, format!("icon-{i}")),
                glyph(item.glyph, 14.0 * scale, ink),
            ));
            if show_copy {
                let word = wrap(
                    key(&self.id, format!("word-{i}")),
                    item.word.clone(),
                    WORD,
                    palette.ink0,
                    &measure,
                    None,
                );
                let mut details = if count_inline {
                    div().flex().items_center().gap(px(gap)).min_w_0().flex_1()
                } else {
                    div().flex().flex_col().items_start().gap(px(gap)).min_w_0()
                };
                details = if stack_icon { details.w_full() } else { details.flex_1() };
                details = if count_inline {
                    details.child(div().min_w_0().flex_1().child(word))
                } else {
                    details.child(word)
                };
                if let Some(count) = item.count {
                    details = details.child(one(key(&self.id, format!("count-{i}")), count.to_string(), COUNT, ink, &measure));
                }
                inner = inner.child(details);
            }
            let tile = cut()
                .chamfer(Chamfer::Px(8.0 * scale))
                .edge(edge_of(item.tone, palette))
                .plate(Plate::Flat)
                .fill(palette.g1)
                .flex_none()
                .min_w_0()
                .w(px(width))
                .min_h(px(chip))
                .overflow_hidden()
                .child(inner);
            let tile = if i == 0 {
                tile.into_any_element()
            } else {
                tile.ml(px(-overlap * (1.0 - open.min(1.0))))
                    .into_any_element()
            };
            hand = hand.child(probe::measure(key(&self.id, format!("chip-{i}")), tile));
        }

        let mut edge = Edge::of(Bevel::Rest, palette);
        edge.hi = palette.line3.into();
        edge.lo = palette.line2.into();
        let edge = edge.mix(Edge::of(Bevel::Peri, palette), open.min(1.0));
        let plate = super::crest::cell(&self.id, "Heads-up", (warns > 0).then(|| warns.to_string()), Some("from its source"), &measure, px(plate_width), window, palette)
            .edge(edge)
            .fill(mix(palette.plate.into(), palette.plate2.into(), open.min(1.0)))
            .w(px(plate_width))
            .min_h(base_h)
            .child(hand)
            .id(key(&self.id, "control"));
        let package = self.package.clone();
        let for_sheet = findings.clone();
        let plate = wire(plate, &touch, Some(Rc::new(move |window: &mut Window, cx: &mut App| open_sheet(&package, for_sheet.clone(), window, cx))));
        // The animated hand's wrapped row heights participate in this
        // natural flow, keeping every following card below the open content.
        div().id(self.id.clone()).flex_none().w(px(plate_width)).child(hover_zone(plate, &touch, 9.0 * scale, true)).into_any_element()
    }
}
