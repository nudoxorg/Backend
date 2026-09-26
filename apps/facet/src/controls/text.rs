//! Measuring a label before layout, so a control can place a sliding plate
//! (the seg's selection, the altimeter's current depth) in the same frame
//! the labels are laid out: no one-frame lag, no pop on the first frame.

use crate::fonts;
use crate::measure::Measure;
use crate::tokens::TypeRole;
use gpui::{Font, FontStyle, FontWeight, Pixels, SharedString, TextRun, Window, black, px};

/// The advance width of `text` set in `role` for `measure` (density, text
/// scale), exactly as a `div().set(role, measure)` would lay it out.
pub(crate) fn width(text: &str, role: TypeRole, measure: &Measure, window: &Window) -> Pixels {
    if text.is_empty() {
        return px(0.0);
    }
    let role = measure.role(role);
    let size = px(role.size);
    let tracking = role.tracking * role.size;
    let run = TextRun {
        len: text.len(),
        font: Font {
            family: SharedString::new_static(fonts::family(role)),
            features: fonts::features(role),
            fallbacks: None,
            weight: FontWeight(role.weight),
            style: if role.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
        },
        color: black(),
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: (tracking != 0.0).then(|| px(tracking)),
    };
    window
        .text_system()
        .shape_line(SharedString::from(text.to_owned()), size, &[run], None)
        .width
}
