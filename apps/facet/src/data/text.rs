//! Text inside a custom-painted mark: one shaped line per run, painted on a
//! shared baseline. Marks never spawn a div per word; they shape their few
//! words here and paint them in the same pass as their geometry.

use crate::fonts;
use crate::tokens::TypeRole;
use gpui::{
    App, Font, FontStyle, FontWeight, Hsla, SharedString, ShapedLine, TextAlign, TextRun, Window,
    point, px,
};

/// One shaped run of text in one role and colour.
#[derive(Clone, Debug)]
pub struct Shaped {
    line: ShapedLine,
    /// The (already measured) role it was shaped in.
    pub role: TypeRole,
}

impl Shaped {
    /// Advance width in px.
    #[must_use]
    pub fn width(&self) -> f32 {
        f32::from(self.line.width())
    }

    /// Ascent above the baseline, px.
    #[must_use]
    pub fn ascent(&self) -> f32 {
        f32::from(self.line.ascent)
    }

    /// Descent below the baseline, px.
    #[must_use]
    pub fn descent(&self) -> f32 {
        f32::from(self.line.descent)
    }

    /// The words shaped.
    #[must_use]
    pub fn text(&self) -> SharedString {
        self.line.text.clone()
    }

    /// The role's line height, px.
    #[must_use]
    pub fn line_height(&self) -> f32 {
        self.role.line
    }

    /// Paints with the left edge at `x` and the baseline at `baseline`.
    pub fn paint(&self, x: f32, baseline: f32, window: &mut Window, cx: &mut App) {
        let height = self.ascent() + self.descent();
        // A glyph the font cannot draw only loses that glyph.
        self.line
            .paint(
                point(px(x), px(baseline - self.ascent())),
                px(height),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .ok();
    }

    /// Paints centred on `cx_` horizontally.
    pub fn paint_centered(&self, centre: f32, baseline: f32, window: &mut Window, cx: &mut App) {
        self.paint(centre - self.width() * 0.5, baseline, window, cx);
    }
}

/// The font a role resolves to.
#[must_use]
pub fn font(role: TypeRole) -> Font {
    Font {
        family: SharedString::new_static(fonts::family(role)),
        features: fonts::features(role),
        fallbacks: None,
        weight: FontWeight(role.weight),
        style: if role.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        },
    }
}

/// Shapes `text` (one line) in `role` (already resolved by a `Measure`).
#[must_use]
pub fn shape(text: impl Into<SharedString>, role: TypeRole, color: Hsla, window: &Window) -> Shaped {
    let text: SharedString = text.into();
    let text = if text.contains('\n') {
        SharedString::from(text.replace('\n', " "))
    } else {
        text
    };
    let spacing = role.tracking * role.size;
    let run = TextRun {
        len: text.len(),
        font: font(role),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: (spacing.abs() > f32::EPSILON).then(|| px(spacing)),
    };
    let line = window
        .text_system()
        .shape_line(text, px(role.size), &[run], None);
    Shaped { line, role }
}

/// Shapes `text`, cutting it with an ellipsis so it fits `max` px.
#[must_use]
pub fn shape_fit(
    text: &str,
    role: TypeRole,
    color: Hsla,
    max: f32,
    window: &Window,
) -> Shaped {
    let full = shape(text.to_owned(), role, color, window);
    if full.width() <= max || text.is_empty() {
        return full;
    }
    // Binary search on char count: shaping is cached, and labels are short.
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid].iter().collect::<String>() + "…";
        if shape(candidate, role, color, window).width() <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let cut: String = chars[..lo].iter().collect::<String>().trim_end().to_owned() + "…";
    shape(cut, role, color, window)
}
