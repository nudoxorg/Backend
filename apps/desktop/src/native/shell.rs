//! Native application shell and shared presentation primitives.

use super::{
    Animation, AnimationExt, Context, DocBlock, Duration, FluentBuilder, Hsla, InteractiveElement,
    IntoElement, NativeApp, ParentElement, Render, Role, Route, SharedString, Styled, Window,
    a11y_key, div, ease_out_quint, host_mode_label, px, revision_short, rgb,
};
use gpui::StatefulInteractiveElement;

impl Render for NativeApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.route {
            Route::Package => self.package_page(cx),
            Route::Docs => self.docs_shell(cx),
            Route::Search => self.search_page(cx),
            Route::Source => self.source_view(cx),
        };
        let route_key = match self.route {
            Route::Package => format!("package-{}", self.package_tab.label()),
            Route::Docs => format!(
                "docs-{}-{}",
                self.docs_filter.as_deref().unwrap_or("all"),
                self.selected.as_deref().unwrap_or("crate")
            ),
            Route::Search => "search".to_owned(),
            Route::Source => "source".to_owned(),
        };
        let content = div()
            .id(SharedString::from(format!("route-content-{route_key}")))
            .size_full()
            .child(content)
            .with_animation(
                "route-enter",
                Animation::new(Duration::from_millis(140)).with_easing(ease_out_quint()),
                |element, delta| element.opacity(delta),
            );
        div()
            .id("application")
            .on_action(cx.listener(Self::focus_global_search))
            .role(Role::Application)
            .aria_label("Nudox local documentation browser")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x000d_1117))
            .text_color(rgb(0x00e8_edf6))
            .when(
                matches!(self.route, Route::Package | Route::Search),
                |shell| shell.child(self.top_bar(cx)),
            )
            .when(matches!(self.route, Route::Docs | Route::Source), |shell| {
                shell.child(self.docs_top_bar(cx))
            })
            .child(
                div()
                    .id("main-content")
                    .role(Role::Main)
                    .aria_label("Package workspace")
                    .flex_1()
                    .min_h_0()
                    .child(content),
            )
            .child(
                div()
                    .id("live-status")
                    .role(Role::Status)
                    .aria_label("Index status")
                    .h(px(28.0))
                    .flex_none()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(rgb(0x0027_303d))
                    .text_size(px(11.0))
                    .text_color(rgb(0x0071_8096))
                    .child(format!("{} · {}", self.status, host_mode_label(self.mode)))
                    .child(format!(
                        "revision {} · {} indexed rows",
                        revision_short(self.root.version().as_bytes()),
                        self.root.row_count()
                    )),
            )
    }
}

pub(super) fn badge(label: &str, background: u32, foreground: u32) -> gpui::AnyElement {
    div()
        .px_2()
        .py_1()
        .rounded_lg()
        .bg(rgb(background))
        .text_size(px(11.0))
        .text_color(rgb(foreground))
        .child(label.to_owned())
        .into_any_element()
}

pub(super) fn side_card(title: &'static str, lines: Vec<String>) -> gpui::AnyElement {
    div()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(rgb(0x002b_394b))
        .bg(rgb(0x0013_1b25))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(0x00e0_e8f4))
                .child(title),
        )
        .children(lines.into_iter().map(|line| {
            div()
                .text_size(px(12.0))
                .text_color(rgb(0x0087_95a9))
                .child(line)
        }))
        .into_any_element()
}

pub(super) fn version_card(
    name: &str,
    revision: String,
    description: &str,
    active: bool,
) -> gpui::AnyElement {
    div()
        .p_5()
        .rounded_lg()
        .border_1()
        .border_color(if active {
            rgb(0x0076_511d)
        } else {
            rgb(0x002b_3849)
        })
        .bg(if active {
            rgb(0x0020_1a12)
        } else {
            rgb(0x0013_1b25)
        })
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(16.0))
                        .text_color(rgb(0x00e1_e9f6))
                        .child(name.to_owned()),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(rgb(0x008e_9caf))
                        .child(description.to_owned()),
                ),
        )
        .child(
            div()
                .font_family("SF Mono")
                .text_size(px(12.0))
                .text_color(rgb(0x00bc_d8ff))
                .child(revision),
        )
        .into_any_element()
}

pub(super) fn section_heading(title: &str, anchor: &str) -> gpui::AnyElement {
    div()
        .id(format!("section-{}", anchor.trim_start_matches('#')))
        .role(Role::Heading)
        .aria_label(title)
        .aria_level(2)
        .text_size(px(20.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(0x00e4_eaf4))
        .child(title.to_owned())
        .into_any_element()
}

pub(super) fn code_block(label: &str, code: SharedString, foreground: Hsla) -> gpui::AnyElement {
    div()
        .id(format!("code-{}-{}", label, a11y_key(&code)))
        .role(Role::Code)
        .aria_label(label)
        .p_4()
        .rounded_lg()
        .bg(rgb(0x0010_1823))
        .border_1()
        .border_color(rgb(0x002a_3a50))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(px(10.0))
                .text_color(rgb(0x007e_91ab))
                .child(label.to_owned()),
        )
        .child(
            div()
                .font_family("SF Mono")
                .text_size(px(14.0))
                .text_color(foreground)
                .child(code),
        )
        .into_any_element()
}

pub(super) fn doc_block(block: &DocBlock) -> gpui::AnyElement {
    match block {
        DocBlock::Prose(text) => div()
            .text_size(px(16.0))
            .line_height(px(26.0))
            .text_color(rgb(0x00d2_dae7))
            .child(text.clone())
            .into_any_element(),
        DocBlock::Code(code) => code_block("example", code.clone(), Hsla::from(rgb(0x00c4_d9f8))),
        DocBlock::Link(label) => div()
            .px_3()
            .py_2()
            .rounded_lg()
            .bg(rgb(0x0017_2941))
            .text_size(px(13.0))
            .text_color(rgb(0x00af_d0ff))
            .child(label.clone())
            .into_any_element(),
        DocBlock::Break => div().h(px(5.0)).into_any_element(),
    }
}

pub(super) fn back_button(cx: &mut Context<NativeApp>) -> impl IntoElement {
    div()
        .id("source-back")
        .role(Role::Button)
        .aria_label("Back to documentation")
        .px_3()
        .py_2()
        .rounded_lg()
        .bg(rgb(0x0020_2a39))
        .text_size(px(12.0))
        .text_color(rgb(0x00c4_d8f2))
        .cursor_pointer()
        .hover(|item| item.bg(rgb(0x002b_3b51)))
        .on_click(cx.listener(|this, _, _, cx| {
            this.route = Route::Docs;
            cx.notify();
        }))
        .child("← Back to docs")
}
