//! Search results and line-numbered source browsing.

use super::{
    Context, FluentBuilder, Hsla, InteractiveElement, IntoElement, NativeApp, ParentElement, Role,
    StatefulInteractiveElement, Styled, back_button, badge, div, px, rgb, short_coordinate,
    source_text_color, uniform_list,
};

impl NativeApp {
    pub(super) fn search_page(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let query = self.search.read(cx).as_str().to_owned();
        div()
            .id("search-page")
            .role(Role::Search)
            .aria_label("Documentation search results")
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(px(980.0))
                    .w_full()
                    .mx_auto()
                    .px_8()
                    .py_8()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .text_size(px(28.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Search documentation"),
                    )
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(rgb(0x009b_a8ba))
                            .child(format!(
                                "{} result{} for “{}”",
                                self.visible.len(),
                                if self.visible.len() == 1 { "" } else { "s" },
                                query
                            )),
                    )
                    .children(self.visible.iter().filter_map(|index| {
                        let row = self.catalog.rows.get(*index)?;
                        let key = row.key.clone();
                        Some(
                            div()
                                .id(format!("search-result-{}", super::a11y_key(&key)))
                                .role(Role::Link)
                                .aria_label(format!("{} {}, {}", row.kind, row.title, row.language))
                                .p_5()
                                .rounded_lg()
                                .border_1()
                                .border_color(rgb(0x002c_394a))
                                .bg(rgb(0x0013_1b25))
                                .cursor_pointer()
                                .hover(|item| item.bg(rgb(0x001d_2836)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.choose(key.clone(), cx);
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .child(badge(&row.kind, 0x0020_2b3d, 0x00ae_cbff))
                                        .child(
                                            div()
                                                .text_size(px(17.0))
                                                .text_color(rgb(0x00e1_e9f6))
                                                .child(row.title.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(12.0))
                                                .text_color(rgb(0x007d_8ca2))
                                                .child(row.language.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .mt_2()
                                        .text_size(px(13.0))
                                        .text_color(rgb(0x008e_9caf))
                                        .child(short_coordinate(&row.coordinate)),
                                )
                                .when(!row.signature.is_empty(), |item| {
                                    item.child(
                                        div()
                                            .mt_3()
                                            .font_family("SF Mono")
                                            .text_size(px(12.0))
                                            .text_color(rgb(0x00b9_cbe3))
                                            .child(row.signature.clone()),
                                    )
                                }),
                        )
                    })),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn source_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.source_loading {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0x00a7_b4c8))
                .child("Loading exact indexed source…")
                .into_any_element();
        }
        if let Some(error) = &self.source_error {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(div().text_color(rgb(0x00f0_a8a8)).child(error.clone()))
                .child(back_button(cx))
                .into_any_element();
        }
        let Some(preview) = &self.source_preview else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0x008b_97aa))
                .child("Choose a declaration to view source")
                .into_any_element();
        };
        let line_count = preview.lines.len();
        let file_name = gpui::SharedString::from(
            preview
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&preview.path)
                .to_owned(),
        );
        div()
            .id("source-view")
            .role(Role::Document)
            .aria_label(format!("Source view for {}", self.package_name()))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .w_full()
                    .h_full()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(118.0))
                            .flex_none()
                            .px_8()
                            .flex()
                            .items_center()
                            .justify_between()
                            .bg(rgb(0x0036_3636))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_5()
                                    .child(back_button(cx))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .font_family("Georgia")
                                                    .text_size(px(18.0))
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child(format!("{}/", self.package_name())),
                                            )
                                            .child(
                                                div()
                                                    .font_family("Georgia")
                                                    .text_size(px(26.0))
                                                    .font_weight(gpui::FontWeight::BOLD)
                                                    .child(file_name),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_7()
                                    .text_size(px(13.0))
                                    .child(
                                        div()
                                            .id("source-search-action")
                                            .role(Role::Button)
                                            .aria_label("Search documentation")
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .cursor_pointer()
                                            .hover(|item| item.bg(rgb(0x0041_4141)))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open_search(window, cx);
                                            }))
                                            .child("⌕ Search"),
                                    )
                                    .child(
                                        div()
                                            .id("source-settings-action")
                                            .role(Role::Button)
                                            .aria_label("Source display settings")
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .cursor_pointer()
                                            .hover(|item| item.bg(rgb(0x0041_4141)))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.status = "Source follows system text and reduced-motion settings".into();
                                                cx.notify();
                                            }))
                                            .child("⚙ Settings"),
                                    )
                                    .child(
                                        div()
                                            .id("source-help-action")
                                            .role(Role::Button)
                                            .aria_label("Source navigation help")
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .cursor_pointer()
                                            .hover(|item| item.bg(rgb(0x0041_4141)))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.status = "Select a line number to identify source; use Command-K to search".into();
                                                cx.notify();
                                            }))
                                            .child("? Help"),
                                    )
                                    .child(
                                        div()
                                            .id("source-summary-action")
                                            .role(Role::Button)
                                            .aria_label("Return to crate summary")
                                            .px_2()
                                            .py_1()
                                            .rounded_sm()
                                            .cursor_pointer()
                                            .hover(|item| item.bg(rgb(0x0041_4141)))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.route = super::Route::Docs;
                                                this.selected = None;
                                                this.docs_filter = None;
                                                cx.notify();
                                            }))
                                            .child("⌄ Summary"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("source-code")
                            .role(Role::Code)
                            .aria_label("Line-numbered source code")
                            .flex_1()
                            .min_h_0()
                            .h_full()
                            .mx_8()
                            .mb_8()
                            .rounded_lg()
                            .bg(rgb(0x002b_2b2b))
                            .overflow_hidden()
                            .child(
                                uniform_list(
                                    "source-code-lines",
                                    line_count,
                                    cx.processor(
                                        |this, range: std::ops::Range<usize>, _window, _cx| {
                                            let Some(preview) = this.source_preview.as_ref() else {
                                                return Vec::new();
                                            };
                                            range
                                                .filter_map(|index| {
                                                    let text = preview.lines.get(index)?.clone();
                                                    let line = u32::try_from(index)
                                                        .ok()?
                                                        .checked_add(1)?;
                                                    let focused = line == preview.line;
                                                    Some(
                                                        div()
                                                            .id(("source-line", line))
                                                            .role(Role::ListItem)
                                                            .aria_label(format!(
                                                                "Line {line}: {text}"
                                                            ))
                                                            .aria_selected(focused)
                                                            .h(px(26.0))
                                                            .min_w_0()
                                                            .px_2()
                                                            .flex()
                                                            .items_center()
                                                            .gap_4()
                                                            .when(focused, |item| {
                                                                item.bg(rgb(0x0045_3b2d))
                                                            })
                                                            .child(
                                                                div()
                                                                    .w(px(58.0))
                                                                    .flex_none()
                                                                    .font_family("SF Mono")
                                                                    .text_size(px(12.0))
                                                                    .text_color(if focused {
                                                                        rgb(0x00ff_c66d)
                                                                    } else {
                                                                        rgb(0x0062_7188)
                                                                    })
                                                                    .child(line.to_string()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .font_family("SF Mono")
                                                                    .text_size(px(13.0))
                                                                    .text_color(if focused {
                                                                        Hsla::from(rgb(0x00ff_e8bd))
                                                                    } else {
                                                                        source_text_color(&text)
                                                                    })
                                                                    .child(text),
                                                            ),
                                                    )
                                                })
                                                .collect()
                                        },
                                    ),
                                )
                                .size_full()
                                .track_scroll(&self.source_scroll),
                            ),
                    ),
            )
            .into_any_element()
    }
}
