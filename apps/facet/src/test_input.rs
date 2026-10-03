//! Complete native key gestures for mounted GPUI control tests.
//!
//! `simulate_keystrokes` dispatches KeyDown only, which is useful for
//! shortcuts but cannot complete a native button or link press. Native
//! activation belongs to the matching release from the same focus owner.

use gpui::{KeyDownEvent, KeyUpEvent, Keystroke, VisualTestContext};

pub(crate) fn native_press(cx: &mut VisualTestContext, key: &str) {
    let keystroke = Keystroke::parse(key).expect("native test key");
    cx.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(KeyUpEvent { keystroke });
}
