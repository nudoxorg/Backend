//! The hover grammar against GPUI's test platform: a pointer move lights the
//! target and every other occurrence of its subject in the frame that
//! answers it, and nothing else.

use super::{FocusTarget, Lit, Subject, hoverable, ink};
use crate::overlay::float;
use crate::theme::{ActiveFacet, Facet, set_facet};
use gpui::{
    Context, IntoElement, Modifiers, MouseExitEvent, ParentElement, Render, Styled, TestAppContext,
    VisualTestContext, Window, div, point, px,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

type Seen = Rc<RefCell<HashMap<&'static str, Lit>>>;

/// Three words: two occurrences of one symbol and one of another.
struct Words {
    seen: Seen,
    /// How far the words sit below the top of the window, px: a test moves
    /// them out from under a still pointer by changing it.
    drop: Rc<Cell<f32>>,
}

impl Render for Words {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.facet().palette();
        let hue = palette.peri.base.hsla();
        let word = |id: &'static str, subject: &'static str, seen: Seen| {
            hoverable(id, Subject::new(subject), hue, move |lit| {
                seen.borrow_mut().insert(id, lit);
                div()
                    .w(px(120.0))
                    .h(px(20.0))
                    .text_color(ink(palette.ink2, lit, palette))
                    .child(id)
                    .into_any_element()
            })
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .pt(px(self.drop.get()))
            .child(word("a", "present::SemanticLinkKind", Rc::clone(&self.seen)))
            .child(word("b", "present::SemanticLinkKind", Rc::clone(&self.seen)))
            .child(word("c", "present::RelationDirection", Rc::clone(&self.seen)))
    }
}

/// The three words in a window, the lit states they last drew, and the knob
/// that drops them down the window.
fn words(cx: &mut TestAppContext) -> (Seen, Rc<Cell<f32>>, &mut VisualTestContext) {
    cx.update(|cx| set_facet(Facet::default(), cx));
    let seen: Seen = Rc::new(RefCell::new(HashMap::new()));
    let drop = Rc::new(Cell::new(0.0));
    let (_view, cx) = cx.add_window_view({
        let (seen, drop) = (Rc::clone(&seen), Rc::clone(&drop));
        |_, _| Words { seen, drop }
    });
    (seen, drop, cx)
}

fn frame(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

#[gpui::test]
fn a_hovered_word_lights_every_other_occurrence_in_the_answering_frame(cx: &mut TestAppContext) {
    let (seen, _drop, cx) = words(cx);
    frame(cx);
    assert_eq!(seen.borrow().get("a"), Some(&Lit::Rest));
    // The pointer arrives on `a` (10 px into it). No clock advances: the
    // frame that answers the move is the frame that lights.
    cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::none());
    frame(cx);
    let lit = seen.borrow().clone();
    assert_eq!(lit.get("a"), Some(&Lit::Target), "the word under the pointer: {lit:?}");
    assert_eq!(lit.get("b"), Some(&Lit::Related), "the other occurrence of its symbol: {lit:?}");
    assert_eq!(lit.get("c"), Some(&Lit::Rest), "another symbol stays at rest: {lit:?}");
    // The pointer moves to `c`: `a` and `b` let go in the same frame.
    cx.simulate_mouse_move(point(px(10.0), px(90.0)), None, Modifiers::none());
    frame(cx);
    let lit = seen.borrow().clone();
    assert_eq!((lit.get("a"), lit.get("b"), lit.get("c")), (Some(&Lit::Rest), Some(&Lit::Rest), Some(&Lit::Target)), "{lit:?}");
    // Leaving the words clears the field.
    cx.simulate_mouse_move(point(px(400.0), px(400.0)), None, Modifiers::none());
    frame(cx);
    assert!(seen.borrow().values().all(|lit| *lit == Lit::Rest), "{:?}", seen.borrow());
}

#[test]
fn ink_rises_one_step_and_strokes_rise_to_the_relation_width() {
    let palette = Facet::default().palette();
    assert_eq!(ink(palette.ink2, Lit::Rest, palette), palette.ink2.hsla());
    assert_eq!(ink(palette.ink2, Lit::Target, palette), palette.ink1.hsla());
    assert_eq!(ink(palette.ink3, Lit::Related, palette), palette.ink2.hsla());
    assert_eq!(ink(palette.ink0, Lit::Target, palette), palette.ink0.hsla(), "the top of the ramp holds");
    assert!((super::stroke(1.2, Lit::Target) - 1.5).abs() < 1e-6);
    assert!((super::stroke(1.2, Lit::Rest) - 1.2).abs() < 1e-6);
}

fn lit_of(seen: &Seen) -> (Option<Lit>, Option<Lit>, Option<Lit>) {
    let seen = seen.borrow();
    (seen.get("a").copied(), seen.get("b").copied(), seen.get("c").copied())
}

/// The pointer can leave the window without a last move: the words it held
/// let go on the exit event (a lit word must not stay lit for ever).
#[gpui::test]
fn leaving_the_window_lets_go_of_the_target(cx: &mut TestAppContext) {
    let (seen, _drop, cx) = words(cx);
    frame(cx);
    cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::none());
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Target), Some(Lit::Related), Some(Lit::Rest)), "the pointer is on a");
    // The exit reports the last position: the word reaches the window's edge, so the
    // pointer leaves from over it and only the exit event can tell the word.
    cx.simulate_event(MouseExitEvent { position: point(px(10.0), px(10.0)), pressed_button: None, modifiers: Modifiers::none() });
    // Before any frame: a window nothing repaints must not keep a lit word.
    assert_eq!(cx.update(|window, cx| super::target(window, cx)), None, "the exit event itself lets go");
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Rest), Some(Lit::Rest), Some(Lit::Rest)), "the pointer left the window");
}

/// A scroll or a reflow moves the word away from a pointer that has not
/// moved: the word lets go, and lights again only when the pointer does.
#[gpui::test]
fn a_word_that_moves_out_from_under_a_still_pointer_lets_go(cx: &mut TestAppContext) {
    let (seen, drop, cx) = words(cx);
    frame(cx);
    cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::none());
    frame(cx);
    assert_eq!(lit_of(&seen).0, Some(Lit::Target));
    drop.set(200.0);
    frame(cx);
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Rest), Some(Lit::Rest), Some(Lit::Rest)), "the pointer is over empty space now");
}

/// Keyboard focus lights a word while the pointer is nowhere near it, and
/// the pointer's own bookkeeping must not let go of it.
#[gpui::test]
fn a_focus_target_survives_frames_with_the_pointer_elsewhere(cx: &mut TestAppContext) {
    let (seen, _drop, cx) = words(cx);
    frame(cx);
    cx.update(|window, cx| super::focus(Some(FocusTarget::new("c", Subject::new("present::RelationDirection"))), window, cx));
    frame(cx);
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Rest), Some(Lit::Rest), Some(Lit::Target)), "focus on c lights c");
    cx.simulate_mouse_move(point(px(400.0), px(400.0)), None, Modifiers::none());
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Rest), Some(Lit::Rest), Some(Lit::Target)), "a pointer move away does not clear keyboard focus");
}

/// Keyboard focus and the pointer use the same element and relation identity;
/// moving focus away releases its light in the answering frame.
#[gpui::test]
fn a_focus_target_lights_its_subject_and_clears_when_focus_leaves(cx: &mut TestAppContext) {
    let (seen, _drop, cx) = words(cx);
    frame(cx);
    cx.update(|window, cx| {
        super::focus(Some(FocusTarget::new("a", Subject::new("present::SemanticLinkKind"))), window, cx);
    });
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Target), Some(Lit::Related), Some(Lit::Rest)));
    cx.update(|window, cx| super::focus(None, window, cx));
    assert_eq!(cx.update(|window, cx| super::target(window, cx)), None, "blur releases a keyboard-held target immediately");
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Rest), Some(Lit::Rest), Some(Lit::Rest)));
}

/// Clearing an absent keyboard target does not erase a pointer target.
#[gpui::test]
fn clearing_keyboard_focus_preserves_the_pointer_target(cx: &mut TestAppContext) {
    let (seen, _drop, cx) = words(cx);
    frame(cx);
    cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::none());
    frame(cx);
    assert_eq!(lit_of(&seen).0, Some(Lit::Target));
    cx.update(|window, cx| super::focus(None, window, cx));
    assert_eq!(cx.update(|window, cx| super::target(window, cx)), Some(("a".into(), Subject::new("present::SemanticLinkKind"))));
}

/// Pointer and keyboard targets are independent per window. A keyboard walk
/// takes the light while focused, then blur restores the still-hovered word;
/// if the pointer leaves first, keyboard focus becomes active again.
#[gpui::test]
fn pointer_and_keyboard_targets_resume_each_other(cx: &mut TestAppContext) {
    let (seen, _drop, cx) = words(cx);
    frame(cx);
    cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::none());
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Target), Some(Lit::Related), Some(Lit::Rest)));

    cx.update(|window, cx| super::focus(Some(FocusTarget::new("b", Subject::new("present::SemanticLinkKind"))), window, cx));
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Related), Some(Lit::Target), Some(Lit::Rest)), "keyboard target takes precedence");

    // The shell may synchronize the same focused target repeatedly. A recent
    // pointer move still takes precedence until it leaves its hitbox.
    cx.simulate_mouse_move(point(px(400.0), px(400.0)), None, Modifiers::none());
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Rest), Some(Lit::Target), Some(Lit::Rest)));
    cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::none());
    cx.update(|window, cx| super::focus(Some(FocusTarget::new("b", Subject::new("present::SemanticLinkKind"))), window, cx));
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Target), Some(Lit::Related), Some(Lit::Rest)), "same-target sync does not steal the pointer light");

    cx.simulate_mouse_move(point(px(400.0), px(400.0)), None, Modifiers::none());
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Rest), Some(Lit::Target), Some(Lit::Rest)), "leaving the pointer restores keyboard focus");

    cx.update(|window, cx| super::focus(None, window, cx));
    assert_eq!(cx.update(|window, cx| super::target(window, cx)), None, "blur clears the last remaining target");
}

#[test]
fn hover_sources_stay_scoped_to_their_window_id() {
    use super::{Field, Held, Source, WindowField};
    use gpui::WindowId;

    let first = WindowId::from(101);
    let second = WindowId::from(202);
    let mut fields = Field::default();
    fields.windows.insert(first, WindowField {
        pointer: Some(Held { id: "pointer-a".into(), subject: Subject::new("a"), source: Source::Pointer }),
        keyboard: None,
        active: Some(Source::Pointer),
    });
    fields.windows.insert(second, WindowField {
        pointer: None,
        keyboard: Some(Held { id: "keyboard-b".into(), subject: Subject::new("b"), source: Source::Keyboard }),
        active: Some(Source::Keyboard),
    });

    assert_eq!(fields.windows.get(&first).and_then(WindowField::held).map(|held| held.id), Some("pointer-a".into()));
    assert_eq!(fields.windows.get(&second).and_then(WindowField::held).map(|held| held.id), Some("keyboard-b".into()));
}

/// Navigation closes what floats and lets go of the hover target: the page
/// under a still pointer is a different page.
#[gpui::test]
fn closing_everything_on_navigation_lets_go_of_the_target(cx: &mut TestAppContext) {
    let (seen, _drop, cx) = words(cx);
    frame(cx);
    cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::none());
    frame(cx);
    assert_eq!(lit_of(&seen).0, Some(Lit::Target));
    cx.update(|window, cx| {
        float::close_all(window, cx);
    });
    assert_eq!(cx.update(|window, cx| super::target(window, cx)), None, "closing everything lets go at once");
    frame(cx);
    assert_eq!(lit_of(&seen), (Some(Lit::Rest), Some(Lit::Rest), Some(Lit::Rest)));
}
