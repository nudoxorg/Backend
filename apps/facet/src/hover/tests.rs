//! The hover grammar against GPUI's test platform: a pointer move lights the
//! target and every other occurrence of its subject in the frame that
//! answers it, and nothing else.

use super::{Lit, Subject, hoverable, ink};
use crate::theme::{ActiveFacet, Facet, set_facet};
use gpui::{
    Context, IntoElement, Modifiers, ParentElement, Render, Styled, TestAppContext, VisualTestContext,
    Window, div, point, px,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

type Seen = Rc<RefCell<HashMap<&'static str, Lit>>>;

/// Three words: two occurrences of one symbol and one of another.
struct Words {
    seen: Seen,
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
            .child(word("a", "present::SemanticLinkKind", Rc::clone(&self.seen)))
            .child(word("b", "present::SemanticLinkKind", Rc::clone(&self.seen)))
            .child(word("c", "present::RelationDirection", Rc::clone(&self.seen)))
    }
}

fn frame(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
}

#[gpui::test]
fn a_hovered_word_lights_every_other_occurrence_in_the_answering_frame(cx: &mut TestAppContext) {
    cx.update(|cx| set_facet(Facet::default(), cx));
    let seen: Seen = Rc::new(RefCell::new(HashMap::new()));
    let (_view, cx) = cx.add_window_view({
        let seen = Rc::clone(&seen);
        |_, _| Words { seen }
    });
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
