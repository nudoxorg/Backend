//! Facet and the component Theme have the same App-global ownership. The
//! existing theme setter projects the effective appearance and palette here;
//! individual documents do not mutate the global theme while rendering.

use super::Facet;
use crate::fonts;
use crate::tokens::{Appearance, Palette, ty};
use gpui::{App, Hsla};
use gpui_component::highlighter::{HighlightTheme, SyntaxColors};
use gpui_component::{Theme, ThemeMode};
use std::sync::Arc;

// The closed mapping is shared by comparison and application. Unowned legacy
// component fields keep their matching appearance's default values.
macro_rules! colors {
    ($palette:ident; $($field:ident: $value:expr),+ $(,)?) => {
        struct Colors { $($field: Hsla),+ }
        impl Colors {
            fn for_palette($palette: &Palette) -> Self {
                Self { $($field: $value),+ }
            }
            fn matches(&self, theme: &Theme) -> bool {
                $(self.$field == theme.$field
                    && theme.tokens.$field.color == self.$field
                    && theme.tokens.$field.background == self.$field.into())&&+
            }
            fn apply(&self, theme: &mut Theme) {
                $(theme.$field = self.$field;
                  theme.tokens.$field = self.$field.into();)+
            }
        }
    };
}

colors! { p;
    background: p.g1.hsla(),
    foreground: p.ink0.hsla(),
    popover: p.plate3.hsla(),
    popover_foreground: p.ink0.hsla(),
    accent: p.plate2.hsla(),
    accent_foreground: p.ink1.hsla(),
    primary: p.mint.base.hsla(),
    primary_active: p.mint.base.hsla(),
    primary_hover: p.mint.base.hsla(),
    primary_foreground: p.mint_ink.hsla(),
    secondary: p.plate2.hsla(),
    secondary_active: p.plate3.hsla(),
    secondary_hover: p.plate3.hsla(),
    secondary_foreground: p.ink1.hsla(),
    muted: p.plate.hsla(),
    muted_foreground: p.ink3.hsla(),
    danger: p.coral.soft.hsla(),
    danger_active: p.coral.soft.hsla(),
    danger_hover: p.coral.soft.hsla(),
    danger_foreground: p.coral.base.hsla(),
    border: p.line1.hsla(),
    input: p.line2.hsla(),
    ring: p.peri.base.hsla(),
    caret: p.mint.base.hsla(),
    selection: alpha(p.peri.base.hsla(), 0.32),
    link: p.peri.base.hsla(),
    link_hover: p.peri_hi.hsla(),
    link_active: p.mint.base.hsla(),
    button: p.plate2.hsla(),
    button_hover: p.plate3.hsla(),
    button_active: p.plate3.hsla(),
    button_foreground: p.ink0.hsla(),
    button_primary: p.mint.base.hsla(),
    button_primary_hover: p.mint.base.hsla(),
    button_primary_active: p.mint.base.hsla(),
    button_primary_foreground: p.mint_ink.hsla(),
    button_secondary: p.plate2.hsla(),
    button_secondary_hover: p.plate3.hsla(),
    button_secondary_active: p.plate3.hsla(),
    button_secondary_foreground: p.ink1.hsla(),
    list: p.g1.hsla(),
    list_hover: p.plate2.hsla(),
    list_active: p.peri.soft.hsla(),
    list_active_border: p.peri.line.hsla(),
    table: p.table.hsla(),
    table_head: p.plate2.hsla(),
    table_head_foreground: p.ink2.hsla(),
    table_row_border: p.line1.hsla(),
    table_hover: p.plate3.hsla(),
}

fn alpha(mut color: Hsla, alpha: f32) -> Hsla {
    color.alpha = alpha;
    color
}

fn syntax(p: &Palette, mode: ThemeMode) -> SyntaxColors {
    let base = if mode.is_dark() {
        HighlightTheme::default_dark()
    } else {
        HighlightTheme::default_light()
    };
    let mut result = base.style.syntax.clone();
    macro_rules! color {
        ($($field:ident: $tone:expr),+ $(,)?) => {
            $(result.$field = Some(result.$field.unwrap_or_default().with_color($tone.hsla()));)+
        };
    }
    color! {
        attribute: p.syntax.attribute,
        boolean: p.syntax.constant,
        comment: p.syntax.comment,
        comment_doc: p.syntax.comment,
        constant: p.syntax.constant,
        constructor: p.syntax.type_name,
        enum_: p.syntax.type_name,
        function: p.syntax.function,
        keyword: p.syntax.keyword,
        link_text: p.peri.base,
        link_uri: p.peri.base,
        number: p.syntax.number,
        operator: p.syntax.punctuation,
        preproc: p.syntax.macro_name,
        property: p.syntax.parameter,
        punctuation: p.syntax.punctuation,
        punctuation_bracket: p.syntax.punctuation,
        punctuation_delimiter: p.syntax.punctuation,
        punctuation_list_marker: p.syntax.punctuation,
        punctuation_special: p.syntax.punctuation,
        string: p.syntax.string,
        string_escape: p.syntax.string,
        string_regex: p.syntax.string,
        string_special: p.syntax.string,
        string_special_symbol: p.syntax.string,
        tag: p.syntax.type_name,
        tag_doctype: p.syntax.attribute,
        text_code_span: p.ink1,
        text_literal: p.ink1,
        title: p.ink0,
        type_: p.syntax.type_name,
        variable: p.syntax.parameter,
        variable_special: p.syntax.macro_name,
        variant: p.syntax.constant,
    }
    result.comment = result
        .comment
        .map(|style| style.with_font_style(gpui_component::highlighter::FontStyle::Italic));
    result.comment_doc = result
        .comment_doc
        .map(|style| style.with_font_style(gpui_component::highlighter::FontStyle::Italic));
    result
}

/// Returns whether the component projection changed. Missing initialization
/// is harmless; reapplying the Facet after component initialization repairs it.
pub(crate) fn sync_components(facet: Facet, cx: &mut App) -> bool {
    if !cx.has_global::<Theme>() {
        return false;
    }
    let mode = match facet.appearance {
        Appearance::Abyss => ThemeMode::Dark,
        Appearance::Glacier => ThemeMode::Light,
    };
    let p = facet.palette();
    let colors = Colors::for_palette(p);
    let syntax = syntax(p, mode);
    let font_size = facet.size(ty::BODY);
    let mono_font_size = facet.size(ty::CODE);
    let theme = Theme::global(cx);
    let highlight = &theme.highlight_theme;
    let highlight_matches = highlight.appearance == mode
        && highlight.style.syntax == syntax
        && highlight.style.editor_foreground == Some(p.ink1.hsla())
        && highlight.style.editor_background == Some(p.plate.hsla());
    if theme.mode == mode
        && colors.matches(theme)
        && theme.font_family.as_str() == fonts::UI
        && theme.mono_font_family.as_str() == fonts::MONO
        && theme.font_size == font_size
        && theme.mono_font_size == mono_font_size
        && highlight_matches
    {
        return false;
    }
    let mode_changed = theme.mode != mode;
    if mode_changed {
        Theme::change(mode, None, cx);
    }
    let theme = Theme::global_mut(cx);
    colors.apply(theme);
    theme.font_family = fonts::UI.into();
    theme.mono_font_family = fonts::MONO.into();
    theme.font_size = font_size;
    theme.mono_font_size = mono_font_size;
    if mode_changed || !highlight_matches {
        let base = if mode.is_dark() {
            HighlightTheme::default_dark()
        } else {
            HighlightTheme::default_light()
        };
        let mut highlight = base.as_ref().clone();
        highlight.name = "FACET".to_owned();
        highlight.style.syntax = syntax;
        highlight.style.editor_foreground = Some(p.ink1.hsla());
        highlight.style.editor_background = Some(p.plate.hsla());
        theme.highlight_theme = Arc::new(highlight);
    }
    Theme::sync_base(cx);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Contrast, set_facet};
    use gpui::TestAppContext;

    fn assert_projection(facet: Facet, cx: &App) {
        let theme = Theme::global(cx);
        let p = facet.palette();
        assert!(Colors::for_palette(p).matches(theme));
        assert_eq!(theme.is_dark(), facet.appearance == Appearance::Abyss);
        assert_eq!(theme.font_family.as_str(), fonts::UI);
        assert_eq!(theme.mono_font_family.as_str(), fonts::MONO);
        assert_eq!(theme.font_size, facet.size(ty::BODY));
        assert_eq!(theme.mono_font_size, facet.size(ty::CODE));
        assert_eq!(theme.foreground, p.ink0.hsla(), "Input ink");
        assert_eq!(theme.caret, p.mint.base.hsla(), "Input caret");
        assert_eq!(theme.selection, alpha(p.peri.base.hsla(), 0.32));
        assert_eq!(theme.accent, p.plate2.hsla(), "Markdown inline code");
        assert_eq!(theme.link, p.peri.base.hsla(), "Markdown link");
        assert_eq!(
            theme.highlight_theme.style.editor_background,
            Some(p.plate.hsla())
        );
        assert_eq!(
            theme
                .highlight_theme
                .style("keyword")
                .and_then(|style| style.color),
            Some(p.syntax.keyword.hsla())
        );
        assert_eq!(
            theme
                .highlight_theme
                .style("comment")
                .and_then(|style| style.color),
            Some(p.syntax.comment.hsla())
        );
        assert_eq!(
            theme
                .highlight_theme
                .style("comment")
                .and_then(|style| style.font_style),
            Some(gpui::FontStyle::Italic)
        );
    }

    #[gpui::test]
    fn effective_appearance_palette_and_scale_reach_input_and_markdown(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        // The setter receives the already resolved system/user preference.
        // This schedule covers boot, appearance changes and high contrast;
        // it does not establish native pixel/owner acceptance.
        for (appearance, contrast, text_scale) in [
            (Appearance::Abyss, Contrast::Normal, 1.0),
            (Appearance::Glacier, Contrast::Normal, 1.0),
            (Appearance::Abyss, Contrast::High, 2.0),
            (Appearance::Glacier, Contrast::High, 1.25),
        ] {
            let facet = Facet {
                appearance,
                contrast,
                text_scale,
                ..Default::default()
            };
            cx.update(|cx| {
                set_facet(facet, cx);
                assert_projection(facet, cx);
                let before = Arc::clone(&Theme::global(cx).highlight_theme);
                assert!(!sync_components(facet, cx));
                set_facet(facet, cx);
                assert!(
                    Arc::ptr_eq(&before, &Theme::global(cx).highlight_theme),
                    "idle repair preserves immutable highlight/cache identity"
                );
            });
        }
    }

    #[gpui::test]
    fn equal_facet_repairs_late_initialization_and_replaced_component_theme(
        cx: &mut TestAppContext,
    ) {
        let facet = Facet::default();
        cx.update(|cx| set_facet(facet, cx));
        cx.update(gpui_component::init);
        cx.update(|cx| {
            assert!(
                !Theme::global(cx).is_dark(),
                "component initialization starts Light"
            );
            set_facet(facet, cx);
            assert_projection(facet, cx);
            Theme::change(ThemeMode::Light, None, cx);
            set_facet(facet, cx);
            assert_projection(facet, cx);
            Theme::global_mut(cx).link = facet.palette().ink4.hsla();
            set_facet(facet, cx);
            assert_projection(facet, cx);
        });
    }
}
