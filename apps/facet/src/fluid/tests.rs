//! The primitive's contract: interpolation, clamping, hysteresis (including a
//! sweep that reverses across a threshold), the mode memory, the grid, and
//! the transition through `facet::motion`.

#![allow(clippy::float_cmp, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::motion::Motion;
use crate::theme::{Facet, set_facet};
use gpui::{Context, IntoElement, Render, Styled, TestAppContext, VisualTestContext, Window, div, px};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

fn room(width: f32) -> Room {
    Room::new(px(width), 1.0)
}

fn px_of(length: &Length, at: Room) -> f32 {
    f32::from(length.at(at))
}

/// A gutter: 16 px at 320, 40 px at 1440.
const GUTTER: Length = Length::new(&[stop(320.0, 16.0), stop(1440.0, 40.0)]);
/// Three end points, eased: what `Measure::space` breathes with.
const BREATHE: Blend = Blend::new(&[stop(320.0, 0.66), stop(480.0, 0.78), stop(1600.0, 1.12)]).smooth();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Dock {
    Drawer,
    Spine,
    Shelf,
}

const DOCK: Ladder<Dock> = Ladder::new(
    ModeId::Dock,
    &[rung(Dock::Drawer, 0.0), rung(Dock::Spine, 640.0), rung(Dock::Shelf, 900.0)],
);

const WIDE_MIN: Length = Length::new(&[stop(320.0, 240.0), stop(1440.0, 300.0)]);
const WIDE: Grid = Grid::new(ModeId::Lab, WIDE_MIN, 3);

/// The gap a caller resolves from its own spacing token.
fn gap() -> gpui::Pixels {
    px(16.0)
}

// ---- tokens: interpolation and clamping ----

#[test]
fn a_token_glides_between_its_ends_and_holds_beyond_them() {
    assert_eq!(px_of(&GUTTER, room(320.0)), 16.0);
    assert_eq!(px_of(&GUTTER, room(880.0)), 28.0, "half way in width is half way in value");
    assert_eq!(px_of(&GUTTER, room(1440.0)), 40.0);
    assert_eq!(px_of(&GUTTER, room(0.0)), 16.0, "below the small end it holds");
    assert_eq!(px_of(&GUTTER, room(100.0)), 16.0);
    assert_eq!(px_of(&GUTTER, room(2560.0)), 40.0, "above the large end it holds");
    assert_eq!(px_of(&GUTTER, room(9000.0)), 40.0);
}

#[test]
fn a_token_never_jumps_and_never_shrinks_as_the_room_grows() {
    // Slope 24 px over 1120 px of width: at most 0.0215 px per px of width.
    let mut last = px_of(&GUTTER, room(0.0));
    for width in 1..3000_u16 {
        let now = px_of(&GUTTER, room(f32::from(width)));
        assert!(now >= last, "shrank at {width}: {last} -> {now}");
        assert!(now - last <= 0.0215, "jumped at {width}: {last} -> {now}");
        last = now;
    }
}

#[test]
fn a_token_is_read_at_the_design_width_and_the_result_scaled() {
    // 1440 px of window at 200 % text is 720 design px, and a length token
    // there is twice what 720 px at 100 % gives.
    let big = Room::new(px(1440.0), 2.0);
    assert_eq!(big.design().get(), 720.0);
    let one = px_of(&GUTTER, room(720.0));
    assert!((px_of(&GUTTER, big) - one * 2.0).abs() < 1e-4, "{} vs {}", px_of(&GUTTER, big), one * 2.0);
    // A factor is read as written: the text scale does not multiply it.
    assert!((BREATHE.at(big) - BREATHE.at(room(720.0))).abs() < 1e-6);
}

#[test]
fn pieces_meet_at_their_stops_and_ease_into_them() {
    assert!((BREATHE.at(room(320.0)) - 0.66).abs() < 1e-6);
    assert!((BREATHE.at(room(480.0)) - 0.78).abs() < 1e-6, "the middle stop is exact");
    assert!((BREATHE.at(room(1600.0)) - 1.12).abs() < 1e-6);
    // Smoothstep: a quarter of the way is 0.15625 of the way in value.
    let quarter = 480.0 + (1600.0 - 480.0) * 0.25;
    let want = 0.78 + (1.12 - 0.78) * 0.156_25;
    assert!((BREATHE.at(room(quarter)) - want).abs() < 1e-5, "{}", BREATHE.at(room(quarter)));
    // And it agrees with the measure's old curve above 480, so no page moves.
    let old = |effective: f32| {
        let raw = ((effective - 480.0) / 1120.0).clamp(0.0, 1.0);
        0.78 + 0.34 * (raw * raw * (3.0 - 2.0 * raw))
    };
    for width in (480..=1700).step_by(7) {
        let width = f32::from(u16::try_from(width).unwrap());
        assert!((BREATHE.at(room(width)) - old(width)).abs() < 1e-5, "differs at {width}");
    }
}

// ---- modes: thresholds and hysteresis ----

#[test]
fn with_no_history_a_ladder_reads_its_plain_edges() {
    assert_eq!(DOCK.at(room(0.0)), Dock::Drawer);
    assert_eq!(DOCK.at(room(639.9)), Dock::Drawer);
    assert_eq!(DOCK.at(room(640.0)), Dock::Spine);
    assert_eq!(DOCK.at(room(899.9)), Dock::Spine);
    assert_eq!(DOCK.at(room(900.0)), Dock::Shelf);
    assert_eq!(DOCK.at(room(5000.0)), Dock::Shelf);
    assert_eq!(DOCK.settle(None, room(899.9)), DOCK.at(room(899.9)));
    // 200 % text on 1440 px is 720 design px: a spine.
    assert_eq!(DOCK.at(Room::new(px(1440.0), 2.0)), Dock::Spine);
}

/// Drives `widths` through a ladder with memory, returning every mode change
/// as (width, from, to).
fn drive(widths: impl IntoIterator<Item = f32>, start: Option<Dock>) -> Vec<(f32, Dock, Dock)> {
    let mut held = start;
    let mut changes = Vec::new();
    for width in widths {
        let now = DOCK.settle(held, room(width));
        if let Some(before) = held.filter(|before| *before != now) {
            changes.push((width, before, now));
        }
        held = Some(now);
    }
    changes
}

fn sweep(from: u16, to: u16) -> Vec<f32> {
    let (lo, hi) = (from.min(to), from.max(to));
    let up: Vec<f32> = (lo..=hi).map(f32::from).collect();
    if from <= to { up } else { up.into_iter().rev().collect() }
}

#[test]
fn a_mode_changes_half_a_band_past_its_edge_going_up_and_going_down() {
    // Widening from a spine: the shelf arrives 16 px past 900.
    let up = drive(sweep(800, 1000), Some(Dock::Spine));
    assert_eq!(up, vec![(916.0, Dock::Spine, Dock::Shelf)]);
    // Narrowing from a shelf: the spine arrives 16 px under 900.
    let down = drive(sweep(1000, 800), Some(Dock::Shelf));
    assert_eq!(down, vec![(883.0, Dock::Shelf, Dock::Spine)]);
    // The same at the lower edge.
    assert_eq!(drive(sweep(560, 760), Some(Dock::Drawer)), vec![(656.0, Dock::Drawer, Dock::Spine)]);
    assert_eq!(drive(sweep(760, 560), Some(Dock::Spine)), vec![(623.0, Dock::Spine, Dock::Drawer)]);
}

#[test]
fn a_sweep_that_reverses_across_a_threshold_flips_once_not_every_frame() {
    // 1000 -> 850 -> 1000 -> 850: two changes down, one up between them, and
    // nothing while it turns around inside the band.
    let widths: Vec<f32> = sweep(1000, 850).into_iter().chain(sweep(850, 1000)).chain(sweep(1000, 850)).collect();
    let changes = drive(widths, Some(Dock::Shelf));
    let path: Vec<Dock> = changes.iter().map(|(_, _, to)| *to).collect();
    assert_eq!(path, vec![Dock::Spine, Dock::Shelf, Dock::Spine]);
    // Reversing inside the band, at every depth of it, never flips.
    for turn in 885..=915_u16 {
        let widths: Vec<f32> = sweep(1000, turn).into_iter().chain(sweep(turn, 1000)).collect();
        assert!(drive(widths, Some(Dock::Shelf)).is_empty(), "flipped when turning at {turn}");
    }
}

#[test]
fn oscillating_ten_px_around_a_threshold_never_flips_it() {
    for edge in [640.0_f32, 900.0] {
        for start in [Dock::Drawer, Dock::Spine, Dock::Shelf] {
            let widths: Vec<f32> = (0..240).map(|frame| edge + if frame % 2 == 0 { -10.0 } else { 10.0 }).collect();
            // From the mode already holding at the edge: no change at all.
            let held = DOCK.at(room(edge));
            let changes = drive(widths.clone(), Some(held));
            assert!(changes.is_empty(), "flipped at {edge} from {held:?}: {changes:?}");
            // From any other mode: at most the one change to the edge's neighbours.
            let changes = drive(widths, Some(start));
            assert!(changes.len() <= 1, "flipped {} times at {edge} from {start:?}", changes.len());
        }
    }
}

#[test]
fn a_random_walk_flips_only_as_often_as_it_crosses_the_whole_band() {
    // A deterministic hand on a window edge: steps of up to 6 px a frame,
    // wandering within 40 px of 900. The mode changes only when the walk has
    // actually gone 16 px past the edge, never on a touch.
    let mut seed = 0x9E37_79B9_u32;
    let mut next = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f32::from(u16::try_from(seed >> 16).unwrap()) / 65_535.0
    };
    let mut at = 900.0_f32;
    let widths: Vec<f32> = (0..20_000)
        .map(|_| {
            at = (at + (next() - 0.5) * 12.0).clamp(860.0, 940.0);
            at
        })
        .collect();
    let changes = drive(widths.clone(), Some(Dock::Spine));
    // Every change is justified by the band: up needs >= 916, down needs < 884.
    for (width, from, to) in &changes {
        match (from, to) {
            (Dock::Spine, Dock::Shelf) => assert!(*width >= 916.0, "up at {width}"),
            (Dock::Shelf, Dock::Spine) => assert!(*width < 884.0, "down at {width}"),
            other => panic!("unexpected change {other:?} at {width}"),
        }
    }
    // A hard threshold would have flipped at every crossing of 900.
    let crossings = widths.windows(2).filter(|pair| (pair[0] < 900.0) != (pair[1] < 900.0)).count();
    assert!(changes.len() * 3 < crossings, "{} changes vs {crossings} crossings of the bare edge", changes.len());
}

#[test]
fn a_fast_drag_across_two_edges_lands_on_the_mode_of_its_room() {
    assert_eq!(DOCK.settle(Some(Dock::Drawer), room(1600.0)), Dock::Shelf);
    assert_eq!(DOCK.settle(Some(Dock::Shelf), room(300.0)), Dock::Drawer);
    assert_eq!(DOCK.settle(Some(Dock::Drawer), room(910.0)), Dock::Spine, "910 is inside the band of 900");
}

// ---- the memory ----

#[test]
fn modes_remember_where_a_region_was_and_count_each_change() {
    let modes = Modes::new();
    let first = modes.settle(&DOCK, room(905.0));
    assert_eq!((first.mode, first.from, first.epoch.count()), (Dock::Shelf, None, 0), "a first read is the plain edge");
    // Inside the band on the way down: held.
    let held = modes.settle(&DOCK, room(890.0));
    assert_eq!((held.mode, held.epoch.count()), (Dock::Shelf, 0));
    // Past it: one change, remembered with what it changed from.
    let spine = modes.settle(&DOCK, room(870.0));
    assert_eq!((spine.mode, spine.from, spine.epoch.count()), (Dock::Spine, Some(Dock::Shelf), 1));
    // Asking again in the same frame is not another change.
    let again = modes.settle(&DOCK, room(870.0));
    assert_eq!((again.mode, again.from, again.epoch.count()), (Dock::Spine, Some(Dock::Shelf), 1));
    // Clones share it, and forgetting resets it to the plain edge.
    let shared = modes.clone();
    assert_eq!(shared.settle(&DOCK, room(895.0)).mode, Dock::Spine);
    modes.forget();
    assert_eq!(shared.settle(&DOCK, room(895.0)).mode, Dock::Spine, "895 is a spine at the plain edge too");
    assert_eq!(shared.settle(&DOCK, room(905.0)).mode, Dock::Spine, "905 is inside the band of 900: a spine holds");
    assert_eq!(shared.settle(&DOCK, room(920.0)).mode, Dock::Shelf);
}

// ---- the grid ----

#[test]
fn a_grid_counts_the_columns_that_fit_and_they_fill_the_room_exactly() {
    for width in (320..2600).step_by(3) {
        let at = room(f32::from(u16::try_from(width).unwrap()));
        let count = WIDE.count(None, at, gap());
        assert!((1..=3).contains(&count), "{count} columns at {width}");
        let column = WIDE.lay(count, at, gap());
        let filled = f32::from(column.width()) * f32::from(u8::try_from(count).unwrap())
            + f32::from(gap()) * f32::from(u8::try_from(count - 1).unwrap());
        assert!((filled - f32::from(at.width())).abs() < 0.01, "{count} columns fill {filled} of {width}");
        if count > 1 {
            let min = px_of(&WIDE_MIN, at);
            assert!(f32::from(column.width()) >= min - 0.01, "column {} under its minimum at {width}", f32::from(column.width()));
        }
    }
}

#[test]
fn a_grid_holds_its_count_through_the_band_and_changes_once() {
    let modes = Modes::new();
    let mut counts = Vec::new();
    let widths = sweep(480, 800).into_iter().chain(sweep(800, 480));
    for width in widths {
        counts.push((width, modes.columns(&WIDE, room(width), gap()).count));
    }
    let changes: Vec<_> = counts.windows(2).filter(|pair| pair[0].1 != pair[1].1).collect();
    // One column to two on the way up, two to one on the way down, and nothing else.
    assert_eq!(changes.len(), 2, "{changes:?}");
    let (up, down) = (changes[0][1].0, changes[1][1].0);
    // Two columns need 2*min + gap (about 515 at 500): up half a band later, down half a band earlier.
    assert!(up > down, "up at {up}, down at {down}: no hysteresis");
    assert!(up - down >= 30.0, "band of {} px", up - down);
    // The columns' epoch counts both.
    assert_eq!(modes.columns(&WIDE, room(480.0), gap()).epoch.count(), 2);
}

// ---- the transition ----

struct Watcher {
    motion: Motion,
    modes: Modes,
    width: f32,
    seen: Rc<Cell<f32>>,
}

impl Render for Watcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settled = self.modes.settle(&DOCK, room(self.width));
        self.seen.set(settled.progress(&self.motion, window, cx));
        div().size_full()
    }
}

fn frame(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.simulate_next_frame(cx);
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

fn advance(cx: &mut VisualTestContext, millis: u64) {
    cx.executor().advance_clock(Duration::from_millis(millis));
    cx.run_until_parked();
}

#[gpui::test]
fn a_mode_change_glides_through_motion_from_zero_to_one(cx: &mut TestAppContext) {
    let seen = Rc::new(Cell::new(f32::NAN));
    let (view, cx) = cx.add_window_view({
        let seen = Rc::clone(&seen);
        |_, _| Watcher { motion: Motion::new(), modes: Modes::new(), width: 1000.0, seen }
    });
    frame(cx);
    assert_eq!(seen.get(), 1.0, "a decision that never changed is settled");
    view.update(cx, |watcher, cx| {
        watcher.width = 600.0;
        cx.notify();
    });
    frame(cx);
    let start = seen.get();
    assert!(start < 0.05, "a change starts at rest: {start}");
    let mut last = start;
    for _ in 0..10 {
        advance(cx, 40);
        frame(cx);
        assert!(seen.get() >= last, "progress is monotone: {last} -> {}", seen.get());
        last = seen.get();
    }
    assert!(last > 0.5, "and it comes: {last}");
    for _ in 0..30 {
        advance(cx, 40);
        frame(cx);
    }
    assert_eq!(seen.get().to_bits(), 1.0_f32.to_bits(), "settles exactly");
}

#[gpui::test]
fn reduced_motion_swaps_at_once_but_stays_hysteretic(cx: &mut TestAppContext) {
    cx.update(|cx| set_facet(Facet { reduced_motion: true, ..Facet::default() }, cx));
    let seen = Rc::new(Cell::new(f32::NAN));
    let (view, cx) = cx.add_window_view({
        let seen = Rc::clone(&seen);
        |_, _| Watcher { motion: Motion::new(), modes: Modes::new(), width: 1000.0, seen }
    });
    frame(cx);
    view.update(cx, |watcher, cx| {
        watcher.width = 600.0;
        cx.notify();
    });
    frame(cx);
    assert_eq!(seen.get(), 1.0, "reduced motion: the swap is instant");
    // Still hysteretic: 890 is inside the band of 900, and a spine holds.
    let modes = view.read_with(cx, |watcher, _| watcher.modes.clone());
    assert_eq!(modes.settle(&DOCK, room(600.0)).mode, Dock::Drawer);
    assert_eq!(modes.settle(&DOCK, room(650.0)).mode, Dock::Drawer, "650 is inside the band of 640");
    assert_eq!(modes.settle(&DOCK, room(660.0)).mode, Dock::Spine);
}

// ---- the rule ----

/// Files (relative to `apps/`) that still compare a width with a number, and
/// whose lane has been told how to stop (`.local/lanes/wave6/fluid/ADOPT.md`).
/// The list only shrinks: a file on it with no hit left fails the test, so a
/// finished file cannot stay listed.
const KNOWN: &[&str] = &[
    // The symbol page (W-Sym6): ADOPT.md, "the symbol page".
    "facet/src/anatomy/gallery.rs",
    "facet/src/anatomy/page.rs",
    "facet/src/anatomy/page/gallery.rs",
    "facet/src/anatomy/prism.rs",
];

/// Files that are checks or harness code, not layout: a width compared with a
/// number there is a measurement, not a breakpoint.
const EXEMPT: &[&str] = &[
    "facet/src/fluid/",
    "facet/src/tokens.rs",
    "facet/src/gallery/",
    "facet/src/graph/gallery",
    "facet/src/graph/sprite_checks.rs",
    "desktop/src/harness",
    "desktop/src/bin/",
];

/// The names a width goes by. A line that compares one of these with a number
/// of three or more digits is a breakpoint.
const WIDTHS: &[&str] = &["width", "effective", ".w ", ".w<", ".w>", " w ", "avail", "vw ", "room."];

/// Whether `line` compares a width with a bare number: a literal of three or
/// more digits (`w < 900.0`, `effective() >= 760.0`, `640.0 <= w`), or a
/// named constant (`effective >= ENTER`, `w >= FOUR_FROM - STICKY`).
fn hand_rolled(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or_default();
    if !WIDTHS.iter().any(|name| code.contains(name)) {
        return false;
    }
    let bytes = code.as_bytes();
    // A named breakpoint a width is compared with (`ENTER`, `ONE_COLUMN_BELOW`,
    // `FOUR_FROM`): an ALL_CAPS name that says which side of an edge it is.
    let constant_at = |from: usize| {
        let name = bytes[from..].iter().take_while(|byte| byte.is_ascii_uppercase() || **byte == b'_').count();
        let word = &code[from..from + name];
        let breakpoint = ["BELOW", "ABOVE", "FROM", "ENTER", "LEAVE", "NARROW", "WIDE", "BREAK"].iter().any(|part| word.contains(part));
        breakpoint && !bytes.get(from + name).is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    };
    // What stands before the operator: the width's name.
    let width_before = |op: usize| {
        let mut end = op;
        while end > 0 && bytes[end - 1] == b' ' {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || matches!(bytes[start - 1], b'_' | b'.' | b'(' | b')')) {
            start -= 1;
        }
        let name = &code[start..end];
        name.ends_with("effective()") || name.ends_with("effective") || name.ends_with("width") || name.ends_with("width()") || name == "w" || name.ends_with(".w")
    };
    let literal_at = |from: usize| {
        let digits = bytes[from..].iter().take_while(|byte| byte.is_ascii_digit()).count();
        digits >= 3 && !bytes.get(from + digits).is_some_and(u8::is_ascii_alphabetic)
    };
    for (at, byte) in bytes.iter().enumerate() {
        if *byte != b'<' && *byte != b'>' {
            continue;
        }
        let (prev, next) = (at.checked_sub(1).map(|i| bytes[i]), bytes.get(at + 1).copied());
        // Not `->`, `=>`, `<<`, `>>`, or a generic's bracket.
        if matches!(prev, Some(b'-' | b'=' | b'<' | b'>')) || matches!(next, Some(b'<' | b'>')) {
            continue;
        }
        let mut after = at + 1;
        if bytes.get(after) == Some(&b'=') {
            after += 1;
        }
        while bytes.get(after) == Some(&b' ') {
            after += 1;
        }
        if literal_at(after) || (constant_at(after) && width_before(at)) {
            return true;
        }
        let mut before = at;
        while before > 0 && bytes[before - 1] == b' ' {
            before -= 1;
        }
        let end = before;
        let mut start = end;
        while start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
            start -= 1;
        }
        let number = &code[start..end];
        if number.split('.').next().is_some_and(|whole| whole.len() >= 3 && whole.bytes().all(|b| b.is_ascii_digit())) {
            return true;
        }
    }
    false
}

/// Every `.rs` file under `dir`.
fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("rs")) {
            out.push(path);
        }
    }
}

/// The offending lines of a source file: the code before its test module (the
/// first `#[cfg(test)]` that is followed by a `mod`; a `#[cfg(test)]` on a
/// statement or a field is not the end of the code).
fn offences(source: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = source.lines().collect();
    let end = lines
        .iter()
        .enumerate()
        .find(|(index, line)| {
            line.trim() == "#[cfg(test)]"
                && lines[index + 1..].iter().map(|next| next.trim()).find(|next| !next.is_empty() && !next.starts_with("#[")).is_some_and(|next| next.starts_with("mod ") || next.starts_with("pub mod "))
        })
        .map_or(lines.len(), |(index, _)| index);
    lines[..end]
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| hand_rolled(line))
        .map(|(index, line)| (index + 1, line.trim().to_owned()))
        .collect()
}

#[test]
fn the_scanner_sees_a_breakpoint_and_only_a_breakpoint() {
    for line in [
        "if view.w < 640.0 { 2.8 } else { 1.9 }",
        "let narrow = measure.effective() < 560.0;",
        "let wide = width >= 900.0;",
        "if 1100.0 <= avail { two }",
        "        room.w < 900.0",
        "let beds = effective>760.0;",
        "Some(true) => effective >= LEAVE,",
        "Columns::Four => w >= FOUR_FROM - STICKY,",
        "env.m.effective() < STACK_BELOW",
    ] {
        assert!(hand_rolled(line), "missed a breakpoint: {line}");
    }
    for line in [
        "let n = width - 24.0 - 36.0;",
        "fn f(x: Vec<u32>) -> Option<f32> { None }",
        "let ok = text.width < 24.0;",
        "if wanted > 500.0 { bold }",
        "let w = width; // was < 900.0",
        "map(|x| x => y)",
        "let a = width << 3;",
        "if width < MAX {",
        "if w < limit {",
    ] {
        assert!(!hand_rolled(line), "flagged what is not a breakpoint: {line}");
    }
}

#[test]
fn no_hand_rolled_breakpoints() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let apps = manifest.parent().expect("apps").to_path_buf();
    let mut files = Vec::new();
    sources(&manifest.join("src"), &mut files);
    sources(&apps.join("desktop/src"), &mut files);
    let mut found: std::collections::BTreeMap<String, Vec<(usize, String)>> = std::collections::BTreeMap::new();
    for path in files {
        let relative = path.strip_prefix(&apps).expect("under apps").to_string_lossy().replace('\\', "/");
        let name = relative.rsplit('/').next().unwrap_or_default();
        if EXEMPT.iter().any(|prefix| relative.starts_with(prefix)) || name.ends_with("tests.rs") || relative.contains("/tests/") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else { continue };
        let hits = offences(&source);
        if !hits.is_empty() {
            found.insert(relative, hits);
        }
    }
    let new: Vec<_> = found.iter().filter(|(file, _)| !KNOWN.contains(&file.as_str())).collect();
    assert!(
        new.is_empty(),
        "a width is compared with a number outside `facet::fluid` (read a token from `facet::tokens::fluid`, or a mode through `Modes::settle`): {new:#?}"
    );
    let stale: Vec<_> = KNOWN.iter().filter(|file| !found.contains_key(**file)).collect();
    assert!(stale.is_empty(), "these files no longer compare a width with a number: take them off `KNOWN`: {stale:?}");
}
