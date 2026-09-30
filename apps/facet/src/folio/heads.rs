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

use super::state::{Build, Library, Nominal, Pose, Unsafe};
use super::text::{ellipsis, key, one, wrap};
use crate::controls::button::wire;
use crate::controls::state::{Touch, hover_zone, track};
use crate::marks::badges::{Glyph, glyph};
use crate::measure::{Measure, Space};
use crate::overlay::dialog::{self, Dialog, DialogButton};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::probe;
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    AnyElement, App, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce, SharedString, Styled, Window,
    deferred, div, px,
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
        let touch = Touch::read(&self.id, crate::controls::Look::LIVE, true, window, cx);
        let motion = touch.motion.clone();
        let wanted = touch.hovered || touch.focused || self.held == Pose::Held;
        let open = motion.animate(track(&self.id, "open"), if wanted { 1.0 } else { 0.0 }, super::state::plate(wanted), window, cx).clamp(0.0, 1.05);
        let findings = self.findings.clone();
        let warns = findings.iter().filter(|f| f.tone == Tone::Warn).count();
        let chip = CHIP * scale;
        let overlap = OVERLAP * scale;
        let gap = f32::from(measure.space(Space::Snug));
        let pad = f32::from(measure.space(Space::Roomy));
        // The step from one chip to the next at rest: the glyphs stay whole
        // unless the cell is too narrow for that many, then they close up.
        let step = rest_step(findings.len(), f32::from(self.width) - pad * 2.0, chip, overlap);

        // Each chip's full width when fanned out: glyph, word, count.
        let word_role = measure.role(WORD);
        let count_role = measure.role(COUNT);
        let widths: Vec<f32> = findings
            .iter()
            .map(|f| {
                let word = f32::from(probe::natural_width(&f.word, word_role, 1.0, window));
                let count = f.count.map_or(0.0, |n| f32::from(probe::natural_width(&SharedString::from(n.to_string()), count_role, 1.0, window)) + gap);
                (10.0 * scale + 14.0 * scale + gap + word + count + 10.0 * scale).max(chip)
            })
            .collect();
        // Fanned out, in rows that fit the room the cell may take.
        let room = f32::from(self.spread) - pad * 2.0;
        let mut spots: Vec<(f32, usize)> = Vec::new();
        let (mut x, mut row) = (0.0_f32, 0usize);
        for w in &widths {
            if x > 0.0 && x + w > room {
                x = 0.0;
                row += 1;
            }
            spots.push((x, row));
            x += w + gap;
        }
        // One finding alone says its words at rest, when its cell holds them: a
        // lone icon in an otherwise empty tile says nothing.
        let lone = findings.len() == 1 && widths.first().is_some_and(|w| *w <= f32::from(self.width) - pad * 2.0);
        let rows = spots.last().map_or(1, |(_, r)| r + 1);
        let spread_used = spots.iter().zip(&widths).map(|((x, _), w)| x + w).fold(0.0, f32::max);
        let plate_w = f32::from(self.width) + (pad * 2.0 + spread_used - f32::from(self.width)).max(0.0) * open.min(1.0);
        let base_h = f32::from(self.height.at(scale));
        let extra = ((rows.saturating_sub(1)) as f32 * (chip + 4.0 * scale)) * open.min(1.0);

        let mut hand = div().relative().h(px(chip + (rows.saturating_sub(1)) as f32 * (chip + 4.0 * scale) * open.min(1.0))).w_full();
        if findings.is_empty() {
            hand = hand.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .h(px(chip))
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Snug))
                    .child(glyph(Glyph::Shield, 16.0 * scale, palette.mint.base))
                    .child(one(key(&self.id, "nothing"), "Nothing to flag", NOTHING, palette.mint.base, &measure)),
            );
        }
        for (i, item) in findings.iter().enumerate() {
            let (spread_x, spread_row) = spots[i];
            #[allow(clippy::cast_precision_loss)]
            let stacked_x = i as f32 * step;
            let x = stacked_x + (spread_x - stacked_x) * open.min(1.0);
            let y = spread_row as f32 * (chip + 4.0 * scale) * open.min(1.0);
            let shown = if lone { 1.0 } else { open.min(1.0) };
            let w = chip + (widths[i] - chip) * shown;
            let ink = ink_of(item.tone, palette);
            let mut inner = div().flex().items_center().h_full().gap(px(gap)).pl(px((chip - 14.0 * scale) * 0.5)).child(glyph(item.glyph, 14.0 * scale, ink));
            if shown > 0.03 {
                inner = inner.child(one(key(&self.id, format!("word-{i}")), item.word.clone(), WORD, palette.ink0, &measure));
                if let Some(count) = item.count {
                    inner = inner.child(one(key(&self.id, format!("count-{i}")), count.to_string(), COUNT, ink, &measure));
                }
            }
            hand = hand.child(
                cut()
                    .chamfer(Chamfer::Px(8.0 * scale))
                    .edge(edge_of(item.tone, palette))
                    .plate(Plate::Flat)
                    .fill(palette.g1)
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w))
                    .h(px(chip))
                    .overflow_hidden()
                    .child(inner),
            );
        }

        let mut edge = Edge::of(Bevel::Rest, palette);
        edge.hi = palette.line3.into();
        edge.lo = palette.line2.into();
        let edge = edge.mix(Edge::of(Bevel::Peri, palette), open.min(1.0));
        let plate = super::crest::cell(&self.id, "Heads-up", (warns > 0).then(|| warns.to_string()), Some("from its source"), &measure, palette)
            .edge(edge)
            .fill(mix(palette.plate.into(), palette.plate2.into(), open.min(1.0)))
            .w(px(plate_w))
            .h(px(base_h + extra))
            .child(hand)
            .id(self.id.clone());
        let package = self.package.clone();
        let for_sheet = findings.clone();
        let plate = wire(plate, &touch, Some(Rc::new(move |window: &mut Window, cx: &mut App| open_sheet(&package, for_sheet.clone(), window, cx))));
        // The hover zone follows the plate as it grows.
        if open > 0.001 || touch.hovered || self.held == Pose::Held {
            // Fanned out it draws above its neighbours (drawn late, hit first).
            let zone = hover_zone(plate.absolute().top_0().left_0(), &touch, 9.0 * scale, true);
            div().relative().flex_none().w(self.width).h(px(base_h)).child(deferred(zone).with_priority(open_priority(open))).into_any_element()
        } else {
            // At rest it is part of the page's own flow, so a page change that
            // cuts the page cuts it too (a deferred draw would not be).
            div().flex_none().w(self.width).h(px(base_h)).child(hover_zone(plate, &touch, 9.0 * scale, true)).into_any_element()
        }
    }
}

/// The step from one chip to the next while the hand is closed: `chip` less
/// `overlap`, unless `room` cannot hold `count` chips that far apart, then as
/// close as the room asks (never under a quarter of a chip).
fn rest_step(count: usize, room: f32, chip: f32, overlap: f32) -> f32 {
    let wide = chip - overlap;
    match count.checked_sub(1).and_then(|n| u16::try_from(n).ok()).filter(|n| *n > 0) {
        Some(gaps) => wide.min(((room - chip) / f32::from(gaps)).max(chip * 0.25)),
        None => wide,
    }
}

/// The plate draws above its neighbours while it is open.
fn open_priority(open: f32) -> usize {
    usize::from(open > 0.01) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chip's glyph sits 7 to 21 px into its 28: the next chip must start
    /// past that, or only the top chip's glyph shows (the stack was unreadable).
    #[test]
    fn a_closed_hand_shows_every_glyph_whole_when_the_cell_holds_them() {
        for count in 1..=6 {
            let step = rest_step(count, 190.0, CHIP, OVERLAP);
            assert!(step >= 21.0, "{count} chips step {step} px, which covers the glyph before it");
        }
    }

    /// In a cell too narrow for the stack it closes up rather than leaving the cell.
    #[test]
    fn a_closed_hand_closes_up_in_a_narrow_cell_and_never_leaves_it() {
        let step = rest_step(6, 100.0, CHIP, OVERLAP);
        assert!(step < 21.0 && step >= CHIP * 0.25, "{step}");
        assert!(step * 5.0 + CHIP <= 100.0 + 0.01, "six chips fit a 100 px cell at step {step}");
    }
}
