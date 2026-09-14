//! Floating surfaces: the settings sheet, hover cards, and the notice bar.
//! Each of them is temporary, elevated, and dismissible with one key.
//! Nothing that reports a failure lives here; failures stay in the page.
//!
//! The hover card is the reason this module exists. A reader scanning a member
//! list wants the signature and the first sentence without losing their place,
//! and the honest way to give them that is a small elevated card with reserved
//! geometry — the frame appears immediately, the content fills in — rather
//! than a card that pops into existence a third of a second late.

use super::workspace::Workspace;
use crate::store::document::HoverCard;
use crate::store::prefs::EditorScheme;
use crate::theme::Theme;
use crate::theme::palette::{Appearance, Paint};
use crate::theme::tokens::{InterfaceSize, Space, TypeScale, space};
use crate::ui::{button, chip, glyph, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, px,
};

/// Width of the hover card.
const CARD: f32 = 380.0;

/// Reserved height of the hover card while its content is arriving.
const CARD_RESERVED: f32 = 64.0;

impl Workspace {
    /// Returns the anchored hover card, when one is showing.
    pub(super) fn hover_card(&self, theme: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let hover = self.document.read(cx).hover()?;
        let anchor = hover.anchor();
        let body = hover.card().map_or_else(
            || reserved(theme).into_any_element(),
            |card| filled(theme, card).into_any_element(),
        );
        Some(
            gpui::anchored()
                .position(gpui::point(anchor.x + px(12.0), anchor.y + px(16.0)))
                .snap_to_window_with_margin(px(8.0))
                .child(
                    surface::raised(theme)
                        .w(px(CARD))
                        .px(space(Space::Room))
                        .py(space(Space::Base))
                        .child(body),
                )
                .into_any_element(),
        )
    }

    /// Returns the short confirmation bar, when one is showing.
    pub(super) fn notice_bar(&self, theme: &Theme, cx: &Context<Self>) -> Option<AnyElement> {
        let notice = self.shell.read(cx).notice()?;
        Some(
            div()
                .absolute()
                .bottom(px(36.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    surface::raised(theme)
                        .flex()
                        .items_center()
                        .gap(space(Space::Snug))
                        .px(space(Space::Room))
                        .py(space(Space::Snug))
                        .child(text::label(theme).child(notice.text().to_owned()))
                        .when_some(notice.detail().map(ToOwned::to_owned), |bar, detail| {
                            bar.child(
                                text::single_line(text::faint(theme))
                                    .max_w(px(360.0))
                                    .font_family(theme.specimen())
                                    .child(detail),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

/// The settings sheet.
impl Workspace {
    /// Returns the settings sheet.
    pub(super) fn settings_sheet(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .inset_0()
            .child(
                surface::scrim(theme)
                    .id("settings-scrim")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell.update(cx, super::super::store::shell::ShellStore::toggle_settings);
                    })),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        surface::raised(theme)
                            .w(px(520.0))
                            .max_h(px(560.0))
                            .p(space(Space::Gutter))
                            .flex()
                            .flex_col()
                            .gap(space(Space::Loose))
                            .child(
                                text::heading(theme, TypeScale::Section).child("Settings"),
                            )
                            .child(self.appearance_setting(theme, cx))
                            .child(self.size_setting(theme, cx))
                            .child(self.motion_setting(theme, cx))
                            .child(self.editor_setting(theme, cx))
                            .child(self.capability_setting(theme, cx))
                            .child(legend_setting(theme)),
                    ),
            )
    }

    fn appearance_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.shell.read(cx).prefs().appearance();
        setting(theme, "Appearance", "The palette this window is lit with.").child(
            div()
                .flex()
                .gap(space(Space::Tight))
                .children([Appearance::Ink, Appearance::Vellum].map(|appearance| {
                    let selected = appearance == current;
                    button::button(
                        theme,
                        format!("appearance-{}", appearance.name()),
                        if appearance.is_dark() { "Ink" } else { "Vellum" },
                        if selected {
                            button::Weight::Primary
                        } else {
                            button::Weight::Regular
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.set_appearance(appearance, cx));
                    }))
                })),
        )
    }

    fn size_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.shell.read(cx).prefs().interface();
        setting(theme, "Interface size", "Scales every measurement in the window.").child(
            div()
                .flex()
                .items_center()
                .gap(space(Space::Snug))
                .child(
                    button::button(theme, "size-down", "−", button::Weight::Regular).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.shell.update(cx, |shell, cx| {
                                let next = shell.prefs().interface().stepped(false);
                                shell.set_interface(next, cx);
                            });
                        }),
                    ),
                )
                .child(
                    text::label(theme)
                        .font_family(theme.specimen())
                        .child(format!("{}%", current.get())),
                )
                .child(
                    button::button(theme, "size-up", "+", button::Weight::Regular).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.shell.update(cx, |shell, cx| {
                                let next = shell.prefs().interface().stepped(true);
                                shell.set_interface(next, cx);
                            });
                        }),
                    ),
                )
                .child(
                    button::button(theme, "size-reset", "Reset", button::Weight::Quiet).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.shell.update(cx, |shell, cx| {
                                shell.set_interface(InterfaceSize::DEFAULT, cx);
                            });
                        }),
                    ),
                ),
        )
    }

    fn motion_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced = self.shell.read(cx).reduced_motion();
        setting(
            theme,
            "Motion",
            "Panels and sheets spring open, or snap into place.",
        )
        .child(
            button::button(
                theme,
                "motion-toggle",
                if reduced { "Snap" } else { "Spring" },
                button::Weight::Regular,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.shell
                    .update(cx, |shell, cx| shell.set_reduced_motion(!reduced, cx));
            })),
        )
    }

    fn editor_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.shell.read(cx).editor();
        setting(
            theme,
            "Open in editor",
            "Where a source site opens when it is followed.",
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(space(Space::Tight))
                .children(EditorScheme::ALL.map(|scheme| {
                    let selected = scheme == current;
                    button::button(
                        theme,
                        format!("editor-{}", scheme.name()),
                        scheme.label(),
                        if selected {
                            button::Weight::Primary
                        } else {
                            button::Weight::Regular
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shell
                            .update(cx, |shell, cx| shell.set_editor(scheme, cx));
                    }))
                })),
        )
    }

    fn capability_setting(&mut self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let chips = self.engine.read(cx).capabilities().to_vec();
        setting(
            theme,
            "Capabilities",
            "What this build can actually compile and search.",
        )
        .child(
            div()
                .id("capability-rows")
                .max_h(px(180.0))
                .overflow_y_scroll()
                .flex()
                .flex_wrap()
                .gap(space(Space::Tight))
                .children(
                    chips
                        .iter()
                        .map(|chip| chip::capability_chip(theme, chip).into_any_element()),
                ),
        )
    }
}

/// Returns the legend: every glyph this window draws, and what it means.
///
/// The interface leans on eighteen lettered kind tiles and eight language
/// tags, all on one luminance plane. That is only readable if a reader can
/// find out, once, what each letter stands for — so the vocabulary is written
/// down inside the product rather than assumed.
fn legend_setting(theme: &Theme) -> Div {
    setting(
        theme,
        "Legend",
        "Every mark this window draws, and what it stands for.",
    )
    .child(
        div()
            .flex()
            .flex_wrap()
            .gap(space(Space::Snug))
            .children(crate::theme::kind::ALL_KINDS.map(|kind| {
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Tight))
                    .child(glyph::kind_tile(theme, Some(kind), false))
                    .child(text::faint(theme).child(glyph::kind_label(Some(kind))))
            })),
    )
    .child(
        div()
            .flex()
            .flex_wrap()
            .gap(space(Space::Snug))
            .children(backend_present::Language::ALL.map(|language| {
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Tight))
                    .child(glyph::language_tag(theme, language))
                    .child(
                        text::faint(theme)
                            .child(crate::theme::language::label(language)),
                    )
            })),
    )
}

fn setting(theme: &Theme, title: &str, detail: &str) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .child(
            text::label(theme)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .child(text::dim(theme).child(detail.to_owned()))
}

fn reserved(theme: &Theme) -> Div {
    div()
        .h(px(CARD_RESERVED))
        .flex()
        .flex_col()
        .justify_center()
        .gap(space(Space::Snug))
        .child(
            div()
                .w(px(220.0))
                .h(px(12.0))
                .rounded_full()
                .bg(theme.paint(Paint::Hover)),
        )
        .child(
            div()
                .w(px(300.0))
                .h(px(10.0))
                .rounded_full()
                .bg(theme.paint(Paint::Hover)),
        )
}

fn filled(theme: &Theme, card: &HoverCard) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .child(
            div()
                .flex()
                .items_center()
                .gap(space(Space::Snug))
                .child(glyph::kind_tile(theme, card.kind(), false))
                .child(
                    text::single_line(text::identity_text(theme, TypeScale::Interface))
                        .child(card.identity().name().to_owned()),
                )
                .child(chip::badge(theme, glyph::kind_label(card.kind()))),
        )
        .child(
            div()
                .font_family(theme.specimen())
                .text_size(crate::theme::tokens::type_size(TypeScale::Small))
                .text_color(theme.paint(Paint::TextDim))
                .child(card.signature().to_owned()),
        )
        .when_some(card.summary().map(ToOwned::to_owned), |body, summary| {
            body.child(text::dim(theme).child(summary))
        })
}
