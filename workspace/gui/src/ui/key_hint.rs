//! `KeyHint` and `KeyCap` — a description plus the keys that perform it.
//!
//! # Why this crate draws its own key caps
//!
//! It used to use `gpui_component::kbd::Kbd`, and `Kbd::format` maps `escape`
//! to **`⎋`** (U+238B, BROKEN CIRCLE WITH NORTHWEST ARROW) on macOS. That is
//! the correct ISO 9995-7 symbol and it is the wrong glyph for this job. At the
//! 11 px caption size the caps are set in, the break in the circle closes up
//! and the arrow flattens into a stub, so the mark renders as a small circle
//! with a tick on it — a clock. The search overlay's footer read
//! `close ⏱`, which is what a reader reported it as before anyone looked at
//! the codepoint.
//!
//! It is not a rendering bug to be worked around; it is a glyph that only works
//! at menu-bar sizes. macOS menus can use it because they set it at 13 px with
//! the system's own hinting. Apple's own keyboards print the word **esc** on
//! the key, and every dense-UI application whose footer hints are legible —
//! zed, Raycast, Linear — prints the word too.
//!
//! # The rule this settles
//!
//! A key cap shows a **symbol** when the symbol is on the physical key and
//! reads at 11 px: the four modifiers (`⌘⌥⌃⇧`), `⏎`, `⌫`, and the arrows. It
//! shows the **word** otherwise, lower-cased, because a lower-case word in a
//! caption row reads as a label and a capitalised one reads as a proper noun.
//! `Kbd`'s table capitalises (`Tab`, `Page Down`), which is why the old footer
//! had `Tab` shouting between two lower-case descriptions.
//!
//! Formatting lives in [`format_key`], which is a pure function over a
//! `Keystroke` so its whole table is unit-testable without a window — and the
//! `⎋` regression has a named test.

use std::rc::Rc;

use gpui::{App, IntoElement, Keystroke, ParentElement, RenderOnce, SharedString, Window, div};
use gpui_component::h_flex;

use crate::platform::ModifierLabels;
use crate::theme::ext::ThemeExtAccessor as _;
use gpui::prelude::*;

// ─────────────────────────────────────────────────────────────────────────────
// Formatting
// ─────────────────────────────────────────────────────────────────────────────

/// Render one keystroke the way a key cap should read at caption size.
///
/// Pure, so the glyph table is testable. See the module docs for the rule.
pub fn format_key(key: &Keystroke) -> String {
    let mut out = String::with_capacity(8);
    let labels = ModifierLabels::current();

    // Modifier order is macOS's own: ⌃⌥⇧⌘ (Apple HT201236). Getting this order
    // wrong is the kind of thing nobody can name and everybody notices. The
    // glyph-vs-word choice per modifier is the platform seam's job
    // (`ModifierLabels`), not this function's.
    if key.modifiers.control {
        out.push_str(labels.control);
    }
    if key.modifiers.alt {
        out.push_str(labels.alt);
    }
    if key.modifiers.shift {
        out.push_str(labels.shift);
    }
    if key.modifiers.platform {
        out.push_str(labels.platform);
    }

    match key.key.as_str() {
        // Symbols that are printed on the physical key and legible at 11 px.
        "enter" => out.push('⏎'),
        "backspace" | "delete" => out.push('⌫'),
        "left" => out.push('←'),
        "right" => out.push('→'),
        "up" => out.push('↑'),
        "down" => out.push('↓'),

        // Words. Lower-case: these sit in a row of lower-case descriptions.
        //
        // `escape` is the one this module exists for. See the module docs.
        "escape" => out.push_str("esc"),
        "tab" => out.push_str("tab"),
        "space" => out.push_str("space"),
        "pageup" => out.push_str("page up"),
        "pagedown" => out.push_str("page down"),
        "home" => out.push_str("home"),
        "end" => out.push_str("end"),

        // A single character is shown upper-case, because that is how it is
        // printed on the key and how every shortcut in every manual writes it.
        other if other.chars().count() == 1 => {
            out.extend(other.chars().flat_map(char::to_uppercase));
        }
        other => out.push_str(other),
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// KeyCap
// ─────────────────────────────────────────────────────────────────────────────

/// One key, drawn as a cap.
///
/// Replaces `gpui_component::kbd::Kbd` so that the cap is themed from this
/// crate's palette and formatted by [`format_key`]. `Kbd` reads
/// gpui-component's `muted`/`muted_foreground`, which — since the palette
/// restructure projects onto those fields — would now be the right colours
/// anyway; the glyph table is why this component exists.
#[derive(IntoElement)]
pub struct KeyCap {
    keystroke: Keystroke,
}

impl KeyCap {
    /// A cap for one keystroke.
    pub fn new(keystroke: Keystroke) -> Self {
        Self { keystroke }
    }
}

impl RenderOnce for KeyCap {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;

        div()
            // A cap is a chip: `r_sm`, the radius the whole system gives
            // chips and badges. It used to take gpui-component's
            // `radius.half()`, which is a different scale entirely, so a key
            // cap and a kind badge sitting three pixels apart in the same
            // footer had different corners.
            .rounded(sp.r_sm)
            .bg(colours.bg_hover)
            .border_1()
            .border_color(colours.border_subtle)
            .text_color(colours.fg_muted)
            .text_size(ts.caption.size)
            .line_height(ts.caption.line_height)
            .px(sp.space_1)
            // A minimum width so `⏎` and `esc` are the same shape of object,
            // and a cap never collapses to a sliver around a narrow glyph.
            .min_w(sp.space_5)
            .text_center()
            .flex_shrink_0()
            .child(SharedString::from(format_key(&self.keystroke)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// KeyHint
// ─────────────────────────────────────────────────────────────────────────────

/// A single keyboard hint: description text + one or more [`KeyCap`]s.
///
/// Used in: palette footer, empty states, `?` shortcuts overlay.
///
/// ```text
///  open        ⏎
///  close       esc
/// ```
#[derive(IntoElement)]
pub struct KeyHint {
    /// Human-readable description of the action.
    description: SharedString,
    /// The keystrokes to display as caps.
    ///
    /// `Rc<[Keystroke]>`, not `Vec<Keystroke>`, because every caller holds its
    /// keystrokes for the lifetime of a view and hands the *same* ones to a new
    /// `KeyHint` on every frame. With a `Vec` that hand-off is a heap
    /// allocation and a `String` clone per cap per row per frame — the search
    /// footer paid five of them a frame and the command palette paid seventy-
    /// nine. A refcount bump is the whole cost now, and the shape makes the
    /// cheap form the default rather than something each caller must remember.
    keys: Rc<[Keystroke]>,
}

impl KeyHint {
    /// Create a hint with a description and a list of keystrokes.
    ///
    /// Takes `impl Into<Rc<[Keystroke]>>` so an owner that has already built
    /// its keystrokes once can pass a clone of the `Rc` (free), while a caller
    /// with a literal `vec![…]` is unchanged.
    pub fn new(description: impl Into<SharedString>, keys: impl Into<Rc<[Keystroke]>>) -> Self {
        Self {
            description: description.into(),
            keys: keys.into(),
        }
    }

    /// Convenience: create from a single keystroke.
    pub fn single(description: impl Into<SharedString>, key: Keystroke) -> Self {
        Self::new(description, vec![key])
    }
}

impl RenderOnce for KeyHint {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;

        h_flex()
            .gap(sp.space_2)
            .items_center()
            // Description label.
            //
            // `fg_faint` from the palette, rather than the previous
            // `muted_foreground.opacity(0.7)`. That expression was one of the
            // fifty-one literal alphas the palette restructure removed, and it
            // was also arithmetic producing a rank the scale already names:
            // "quieter than muted" is `fg_faint`, and computing it per-caller
            // meant the search footer, the palette footer and the shortcuts
            // sheet each landed on a slightly different grey.
            .child(
                div()
                    .text_color(colours.fg_faint)
                    .text_size(ts.caption.size)
                    .line_height(ts.caption.line_height)
                    .child(self.description),
            )
            .children(self.keys.iter().map(|k| KeyCap::new(k.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::format_key;
    use gpui::Keystroke;

    /// The defect this module was written for.
    ///
    /// `⎋` is the correct ISO symbol for Escape and it is illegible at caption
    /// size — it reads as a clock face. If someone reintroduces it, this fails
    /// with the reason rather than waiting for another screenshot review.
    #[test]
    fn escape_is_a_word_and_never_the_broken_circle_glyph() {
        let esc = Keystroke::parse("escape").expect("escape parses");
        let rendered = format_key(&esc);
        assert_eq!(rendered, "esc");
        assert!(
            !rendered.contains('\u{238B}'),
            "U+238B renders as a clock at 11 px; it must never reach a key cap"
        );
    }

    /// Word keys are lower-case so they sit in a row of lower-case hints
    /// without shouting; single characters are upper-case because that is what
    /// is printed on the key.
    #[test]
    fn word_keys_are_lower_case_and_letter_keys_are_upper_case() {
        let cases = [
            ("tab", "tab"),
            ("space", "space"),
            ("pagedown", "page down"),
        ];
        for (input, expected) in cases {
            let k = Keystroke::parse(input).expect("keystroke parses");
            assert_eq!(format_key(&k), expected, "for `{input}`");
        }
        let a = Keystroke::parse("a").expect("keystroke parses");
        assert_eq!(format_key(&a), "A");
    }

    /// Modifier order is macOS's: ⌃⌥⇧⌘, then the key.
    #[test]
    #[cfg(target_os = "macos")]
    fn modifier_order_follows_the_platform_convention() {
        let k = Keystroke::parse("ctrl-alt-shift-cmd-k").expect("keystroke parses");
        assert_eq!(format_key(&k), "⌃⌥⇧⌘K");
        let enter = Keystroke::parse("cmd-enter").expect("keystroke parses");
        assert_eq!(format_key(&enter), "⌘⏎");
    }

    /// Non-macOS: modifiers are words, and the platform modifier is the real
    /// key (super on Linux, win on Windows) — never a hard-coded "win".
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn modifier_order_uses_the_real_platform_word() {
        use crate::platform::ModifierLabels;

        let k = Keystroke::parse("ctrl-alt-shift-cmd-k").expect("keystroke parses");
        let rendered = format_key(&k);
        let labels = ModifierLabels::current();
        let expected = format!(
            "{}{}{}{}K",
            labels.control, labels.alt, labels.shift, labels.platform
        );
        assert_eq!(rendered, expected);
        assert!(
            !rendered.ends_with("win K"),
            "the platform modifier must name the real key, not hard-code \"win\""
        );
    }
}
