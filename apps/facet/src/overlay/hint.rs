//! Hint mode (F): every actionable thing on screen gets a one- or two-letter
//! home-row code; typing narrows the field (codes that no longer match
//! vanish, matched letters drop off the front) until one remains, and it
//! acts. Esc leaves.
//!
//! Components register their actionable rects while they paint:
//!
//! ```ignore
//! hint::target(bounds, move |window, cx| open_page(&id, window, cx), window, cx);
//! ```
//!
//! Registration is a no-op outside hint mode. Entering it repaints the whole
//! window once (so cached regions register too), assigns codes in reading
//! order, and the float layer draws the labels over everything.

use crate::controls::kbd;
use crate::measure::Measure;
use gpui::{
    AnyElement, App, Bounds, Global, IntoElement, Keystroke, ParentElement, Pixels, Styled, Window,
    WindowId, div,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// The home row, in the order codes are handed out.
pub const LETTERS: [char; 9] = ['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l'];

type Action = Rc<dyn Fn(&mut Window, &mut App)>;

/// `n` codes: single letters when they suffice, else two letters each (so
/// no code is a prefix of another). At most 81.
#[must_use]
pub fn codes(n: usize) -> Vec<String> {
    if n <= LETTERS.len() {
        return LETTERS.iter().take(n).map(ToString::to_string).collect();
    }
    LETTERS
        .iter()
        .flat_map(|first| LETTERS.iter().map(move |second| format!("{first}{second}")))
        .take(n)
        .collect()
}

/// Which codes still match what was typed.
#[must_use]
pub fn narrow<'a>(codes: &'a [String], typed: &str) -> Vec<(usize, &'a str)> {
    codes
        .iter()
        .enumerate()
        .filter(|(_, code)| code.starts_with(typed))
        .map(|(index, code)| (index, code.as_str()))
        .collect()
}

/// Reading order: top to bottom, then left to right (rows within 4 px).
pub fn reading_order(rects: &mut [(Bounds<Pixels>, Action)]) {
    rects.sort_by(|(a, _), (b, _)| {
        let (ay, by) = (f32::from(a.origin.y), f32::from(b.origin.y));
        if (ay - by).abs() > 4.0 {
            ay.total_cmp(&by)
        } else {
            f32::from(a.origin.x).total_cmp(&f32::from(b.origin.x))
        }
    });
}

#[derive(Default)]
enum Mode {
    #[default]
    Off,
    Collecting(Vec<(Bounds<Pixels>, Action)>),
    Showing {
        targets: Vec<(Bounds<Pixels>, Action)>,
        codes: Vec<String>,
        typed: String,
    },
}

#[derive(Default)]
struct PerWindow(HashMap<WindowId, Rc<RefCell<Mode>>>);

impl Global for PerWindow {}

fn mode(window: &Window, cx: &mut App) -> Rc<RefCell<Mode>> {
    let id = window.window_handle().window_id();
    cx.default_global::<PerWindow>().0.entry(id).or_default().clone()
}

/// Registers an actionable rect for this frame (no-op outside hint mode).
pub fn target(
    bounds: Bounds<Pixels>,
    action: impl Fn(&mut Window, &mut App) + 'static,
    window: &Window,
    cx: &mut App,
) {
    let mode = mode(window, cx);
    if let Mode::Collecting(targets) = &mut *mode.borrow_mut() {
        targets.push((bounds, Rc::new(action)));
    }
}

/// F: enters hint mode (the next frame collects, the one after shows).
pub fn enter(window: &mut Window, cx: &mut App) {
    *mode(window, cx).borrow_mut() = Mode::Collecting(Vec::new());
    window.refresh();
}

/// Leaves hint mode.
pub fn exit(window: &mut Window, cx: &mut App) {
    *mode(window, cx).borrow_mut() = Mode::Off;
    super::float::refresh(window, cx);
}

/// Whether hint mode is on.
#[must_use]
pub fn is_active(window: &Window, cx: &mut App) -> bool {
    !matches!(*mode(window, cx).borrow(), Mode::Off)
}

/// Called by the float layer at the end of its prepaint: a collecting frame
/// becomes a showing one (codes in reading order); the labels draw next frame.
pub(crate) fn finish_collecting(window: &mut Window, cx: &mut App) {
    let mode = mode(window, cx);
    let mut current = mode.borrow_mut();
    if let Mode::Collecting(targets) = &mut *current {
        let mut targets = std::mem::take(targets);
        reading_order(&mut targets);
        targets.truncate(LETTERS.len() * LETTERS.len());
        let codes = codes(targets.len());
        *current = Mode::Showing {
            targets,
            codes,
            typed: String::new(),
        };
        drop(current);
        crate::motion::request_frame(window, cx);
    }
}

/// A key while hint mode is on: Esc leaves, a home-row letter narrows and,
/// once one code is complete, acts. Returns whether the key was used.
pub fn handle_key(keystroke: &Keystroke, window: &mut Window, cx: &mut App) -> bool {
    let mode_rc = mode(window, cx);
    if matches!(*mode_rc.borrow(), Mode::Off) {
        return false;
    }
    if keystroke.key == "escape" {
        exit(window, cx);
        return true;
    }
    let Some(letter) = keystroke.key.chars().next().filter(|ch| LETTERS.contains(ch) && keystroke.key.len() == 1) else {
        return true; // swallow everything else while hints show
    };
    let chosen = {
        let mut current = mode_rc.borrow_mut();
        let Mode::Showing { targets, codes, typed } = &mut *current else {
            return true;
        };
        let mut next = typed.clone();
        next.push(letter);
        let matches = narrow(codes, &next);
        match matches.as_slice() {
            [] => None,
            [(index, code)] if *code == next => Some(targets[*index].1.clone()),
            _ => {
                *typed = next;
                None
            }
        }
    };
    if let Some(action) = chosen {
        *mode_rc.borrow_mut() = Mode::Off;
        action(window, cx);
    }
    super::float::refresh(window, cx);
    true
}

/// The labels as the layer draws them (absolutely placed at each target's
/// top-left). `None` outside hint mode.
pub(crate) fn element(measure: &Measure, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let mode = mode(window, cx);
    let current = mode.borrow();
    let Mode::Showing { targets, codes, typed } = &*current else {
        return None;
    };
    let mut layer = div().absolute().top_0().left_0().size_full();
    for (index, code) in narrow(codes, typed) {
        let rect = targets[index].0;
        let rest = code[typed.len()..].to_uppercase();
        layer = layer.child(
            div()
                .absolute()
                .left(rect.origin.x - gpui::px(4.0))
                .top(rect.origin.y - gpui::px(4.0))
                .child(kbd(rest, measure).hint()),
        );
    }
    let _ = cx;
    Some(layer.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::{LETTERS, codes, narrow};

    #[test]
    fn few_targets_get_single_letters_many_get_pairs_and_none_is_a_prefix() {
        assert_eq!(codes(3), ["a", "s", "d"]);
        let many = codes(30);
        assert_eq!(many.len(), 30);
        assert!(many.iter().all(|code| code.len() == 2));
        for a in &many {
            for b in &many {
                assert!(a == b || !b.starts_with(a.as_str()), "{a} is a prefix of {b}");
            }
        }
        assert_eq!(codes(200).len(), LETTERS.len() * LETTERS.len());
    }

    #[test]
    fn typing_narrows_to_matching_codes() {
        let all = codes(20);
        let after_s = narrow(&all, "s");
        assert_eq!(after_s.len(), 9);
        assert!(after_s.iter().all(|(_, code)| code.starts_with('s')));
        assert_eq!(narrow(&all, "sd").len(), 1);
        assert!(narrow(&all, "z").is_empty());
    }
}
