//! One declaration page: trail, signature, prose, members, relations, source.
//! Every type name in the signature and every link in the prose is navigable.
//! Absences are stated — a missing source excerpt says which kind of missing.
//!
//! The order is the order a reader asks: what is this called, what shape does
//! it have, what does it say about itself, what does it contain, what touches
//! it, and finally what does it actually look like in the file. Nothing is a
//! card; the page is one column of sections separated by hairlines, which is
//! what lets a dense declaration and a sparse one look like the same document.

use super::workspace::Workspace;
use crate::presentation::identity::Crumb;
use crate::presentation::page::{Member, MemberGroup, Page, RelationGroup, SourceBlock};
use crate::store::document::Target;
use crate::theme::Theme;
use crate::theme::kind::group_title;
use crate::theme::language::Language;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::{button, chip, glyph, prose, specimen, surface, text};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px,
};

/// How many members are drawn before a group offers to show the rest.
const MEMBER_BUDGET: usize = 40;

/// How many source lines are drawn before the block offers to show the rest.
const SOURCE_BUDGET: usize = 36;

impl Workspace {
    /// Returns the whole declaration page.
    pub(super) fn declaration_page(
        &mut self,
        theme: &Theme,
        page: &Page,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Gutter))
            .child(self.page_header(theme, page, cx))
            .when(!page.signature().is_empty(), |body| {
                body.child(self.signature_section(theme, page, cx))
            })
            .when(!page.prose().is_empty(), |body| {
                body.child(self.prose_section(theme, page, cx))
            })
            .children(
                page.members()
                    .iter()
                    .map(|group| self.member_group(theme, group, cx)),
            )
            .when(!page.relations().is_empty(), |body| {
                body.children(
                    page.relations()
                        .iter()
                        .map(|group| self.relation_group(theme, group, cx)),
                )
            })
            .child(self.source_section(theme, page, cx))
    }

    fn page_header(
        &mut self,
        theme: &Theme,
        page: &Page,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let identity = page.identity().clone();
        let key = page.key().clone();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(self.trail(theme, page, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(glyph::kind_tile(theme, page.kind(), true))
                    .child(
                        text::heading(theme, TypeScale::Title)
                            .flex_none()
                            .child(identity.name().to_owned()),
                    )
                    .child(chip::badge(theme, glyph::kind_label(page.kind())))
                    .child(glyph::language_tag(theme, page.language()))
                    .child(div().flex_1())
                    .child(self.copy_controls(theme, &identity, &key, cx)),
            )
    }

    fn trail(&mut self, theme: &Theme, page: &Page, cx: &mut Context<Self>) -> impl IntoElement {
        let identity = page.identity().clone();
        let crumbs = identity.trail();
        let project = identity.project().to_owned();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space(Space::Tight))
            .children(crumbs.into_iter().enumerate().map(|(at, crumb)| {
                let separator = (at > 0).then(|| {
                    text::faint(theme)
                        .flex_none()
                        .text_color(theme.paint(Paint::GiltDim))
                        .child("›")
                        .into_any_element()
                });
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Tight))
                    .when_some(separator, ParentElement::child)
                    .child(self.crumb(theme, at, &crumb, &project, cx))
                    .into_any_element()
            }))
    }

    fn crumb(
        &mut self,
        theme: &Theme,
        at: usize,
        crumb: &Crumb,
        project: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = crumb.label().to_owned();
        let project = project.to_owned();
        let opens_project = matches!(crumb, Crumb::Project(_));
        div()
            .id(ElementId::Name(SharedString::from(format!("crumb-{at}"))))
            .cursor_pointer()
            .child(
                text::identity_text(theme, TypeScale::Small)
                    .child(text::elide(&label, 42).to_string()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if opens_project {
                    this.open_project(project.clone(), cx);
                }
            }))
            .into_any_element()
    }

    fn copy_controls(
        &mut self,
        theme: &Theme,
        identity: &crate::presentation::identity::Identity,
        key: &crate::presentation::identity::KeyTag,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let coordinate = identity.coordinate().to_owned();
        let full = key.full().to_owned();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(space(Space::Tight))
            .child(
                button::button(theme, "copy-identity", "Copy identity", button::Weight::Quiet)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy("Identity copied", coordinate.clone(), cx);
                    })),
            )
            .child(
                div()
                    .id("key-tag")
                    .px(space(Space::Snug))
                    .py(px(1.0))
                    .rounded(radius(Radius::Hair))
                    .bg(theme.paint(Paint::GiltWash))
                    .font_family(theme.specimen())
                    .text_size(type_size(TypeScale::Micro))
                    .text_color(theme.paint(Paint::Gilt))
                    .cursor_pointer()
                    .child(key.short().to_owned())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy("Key copied", full.clone(), cx);
                    })),
            )
    }

    fn signature_section(
        &mut self,
        theme: &Theme,
        page: &Page,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let document = self.document.clone();
        section(
            theme,
            "Signature",
            specimen::signature_block(theme, page.signature(), "page-signature", move |symbol, _, cx| {
                open_symbol(&document, symbol, cx);
            })
            .into_any_element(),
        )
        .child(div())
    }

    fn prose_section(
        &mut self,
        theme: &Theme,
        page: &Page,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let document = self.document.clone();
        section(
            theme,
            "Documentation",
            prose::blocks(theme, page.prose(), "page", move |symbol, _, cx| {
                open_symbol(&document, symbol, cx);
            })
            .into_any_element(),
        )
    }

    fn member_group(
        &mut self,
        theme: &Theme,
        group: &MemberGroup,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = group_title(group.kind());
        let members = group.members();
        let shown = members.len().min(MEMBER_BUDGET);
        let body = div()
            .w_full()
            .flex()
            .flex_col()
            .children(
                members
                    .iter()
                    .take(shown)
                    .enumerate()
                    .map(|(at, member)| self.member_row(theme, title, at, member, cx)),
            )
            .when(members.len() > shown, |body| {
                body.child(
                    text::faint(theme)
                        .pt(space(Space::Tight))
                        .child(format!("{} more not shown", members.len() - shown)),
                )
            })
            .into_any_element();
        section_with_count(theme, title, members.len(), body).into_any_element()
    }

    fn member_row(
        &mut self,
        theme: &Theme,
        group: &str,
        at: usize,
        member: &Member,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let symbol = member.symbol();
        let coordinate = member.identity().coordinate().to_owned();
        let summary = member.summary().map(ToOwned::to_owned);
        div()
            .id(ElementId::Name(SharedString::from(format!(
                "member-{group}-{at}"
            ))))
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .py(px(3.0))
            .px(space(Space::Snug))
            .rounded(radius(Radius::Hair))
            .min_w(px(0.0))
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                let target = super::reader::modifier_target(event.modifiers().alt);
                this.open_symbol(symbol, target, cx);
            }))
            .on_hover(cx.listener(move |this, entering: &bool, window, cx| {
                this.hover_member(symbol, &coordinate, *entering, window, cx);
            }))
            .child(glyph::kind_tile(theme, member.kind(), false))
            .child(
                text::single_line(text::navigable(theme, TypeScale::Interface, false))
                    .flex_none()
                    .max_w(px(240.0))
                    .child(member.identity().name().to_owned()),
            )
            .child(
                text::single_line(text::dim(theme))
                    .flex_1()
                    .min_w(px(0.0))
                    .font_family(theme.specimen())
                    .child(member.preview().to_owned()),
            )
            .when_some(summary, |row, summary| {
                row.child(
                    text::single_line(text::faint(theme))
                        .flex_none()
                        .max_w(px(220.0))
                        .child(summary),
                )
            })
            .into_any_element()
    }

    fn relation_group(
        &mut self,
        theme: &Theme,
        group: &RelationGroup,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = group.label();
        let body = div()
            .flex()
            .flex_wrap()
            .gap(space(Space::Tight))
            .children(
                group
                    .entries()
                    .iter()
                    .enumerate()
                    .map(|(at, entry)| self.relation_chip(theme, label, at, entry, cx)),
            )
            .into_any_element();
        section_with_count(theme, label, group.entries().len(), body).into_any_element()
    }

    fn relation_chip(
        &mut self,
        theme: &Theme,
        group: &str,
        at: usize,
        entry: &Member,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let symbol = entry.symbol();
        div()
            .id(ElementId::Name(SharedString::from(format!(
                "relation-{group}-{at}"
            ))))
            .flex()
            .flex_none()
            .items_center()
            .gap(space(Space::Tight))
            .px(space(Space::Snug))
            .py(px(2.0))
            .rounded(radius(Radius::Hair))
            .bg(theme.paint(Paint::Hover))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Selected)))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                let target = super::reader::modifier_target(event.modifiers().alt);
                this.open_symbol(symbol, target, cx);
            }))
            .child(glyph::kind_tile(theme, entry.kind(), false))
            .child(
                text::label(theme)
                    .text_size(type_size(TypeScale::Small))
                    .child(entry.identity().name().to_owned()),
            )
            .into_any_element()
    }
}

/// The source section.
impl Workspace {
    fn source_section(
        &mut self,
        theme: &Theme,
        page: &Page,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let body = match page.source() {
            SourceBlock::Captured {
                path,
                start,
                lines,
                truncated,
            } => self.source_block(theme, path, *start, lines, *truncated, cx),
            other => absence(theme, other).into_any_element(),
        };
        section(theme, "Source", body)
    }

    fn source_block(
        &mut self,
        theme: &Theme,
        path: &str,
        start: u32,
        lines: &[crate::presentation::page::SourceLine],
        truncated: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = lines.len().min(SOURCE_BUDGET);
        let site = format!("{path}:{start}");
        surface::sunken(theme)
            .w_full()
            .overflow_hidden()
            .child(self.source_head(theme, path, start, &site, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(space(Space::Base))
                    .py(space(Space::Snug))
                    .children(lines.iter().take(shown).map(|line| {
                        source_line(theme, line.number(), line.text(), line.number() == start)
                    }))
                    .when(lines.len() > shown || truncated, |body| {
                        body.child(
                            text::faint(theme)
                                .pt(space(Space::Tight))
                                .child(remainder(lines.len(), shown, truncated)),
                        )
                    }),
            )
            .into_any_element()
    }

    fn source_head(
        &mut self,
        theme: &Theme,
        path: &str,
        line: u32,
        site: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let target = path.to_owned();
        let site_text = site.to_owned();
        div()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .px(space(Space::Base))
            .py(px(4.0))
            .border_b(hairline())
            .border_color(theme.paint(Paint::Hairline))
            .child(
                text::single_line(text::faint(theme))
                    .flex_1()
                    .min_w(px(0.0))
                    .font_family(theme.specimen())
                    .child(site_text.clone()),
            )
            .child(
                button::button(theme, "open-editor", "Open in editor", button::Weight::Quiet)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_in_editor(&target, line, cx);
                    })),
            )
            .child(
                button::button(theme, "copy-site", "Copy path", button::Weight::Quiet).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.copy("Path copied", site_text.clone(), cx);
                    }),
                ),
            )
    }

    /// Opens one source site in the configured external editor.
    pub(super) fn open_in_editor(&mut self, path: &str, line: u32, cx: &mut Context<Self>) {
        let project = self.workspace.read(cx).project().to_path_buf();
        let candidate = std::path::Path::new(path);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            project.join(candidate)
        };
        match self.shell.read(cx).editor().url(&resolved, line) {
            Some(url) => cx.open_url(&url),
            None => cx.reveal_path(&resolved),
        }
    }

    fn hover_member(
        &mut self,
        symbol: backend_library::SymbolKey,
        coordinate: &str,
        entering: bool,
        window: &gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if !entering {
            self.document
                .update(cx, |document, cx| document.clear_hover(cx));
            return;
        }
        let anchor = window.mouse_position();
        let coordinate = coordinate.to_owned();
        self.document.update(cx, |document, cx| {
            document.hover_over(symbol, coordinate, anchor, cx);
        });
    }
}

fn open_symbol(
    document: &Entity<crate::store::document::DocumentStore>,
    symbol: backend_library::SymbolKey,
    cx: &mut gpui::App,
) {
    document.update(cx, |document, cx| {
        let coordinate = backend_library::encode_id(symbol.as_bytes());
        document.open(
            crate::store::document::Subject::Declaration { symbol, coordinate },
            Target::Here,
            cx,
        );
    });
}

fn section(theme: &Theme, title: &str, body: AnyElement) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .child(section_head(theme, title, None))
        .child(body)
}

fn section_with_count(theme: &Theme, title: &str, count: usize, body: AnyElement) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(space(Space::Snug))
        .child(section_head(theme, title, Some(count)))
        .child(body)
}

fn section_head(theme: &Theme, title: &str, count: Option<usize>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(space(Space::Snug))
        .child(
            text::faint(theme)
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_ascii_uppercase()),
        )
        .when_some(count, |head, count| {
            head.child(text::faint(theme).child(count.to_string()))
        })
        .child(
            div()
                .flex_1()
                .h(hairline())
                .bg(theme.paint(Paint::Hairline)),
        )
}

fn source_line(theme: &Theme, number: u32, body: &str, current: bool) -> Div {
    div()
        .flex()
        .gap(space(Space::Base))
        .when(current, |line| line.bg(theme.paint(Paint::GiltWash)))
        .child(
            div()
                .flex_none()
                .w(px(38.0))
                .text_align(gpui::TextAlign::Right)
                .font_family(theme.specimen())
                .text_size(type_size(TypeScale::Micro))
                .text_color(theme.paint(Paint::TextFaint))
                .child(number.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .whitespace_nowrap()
                .overflow_hidden()
                .font_family(theme.specimen())
                .text_size(type_size(TypeScale::Small))
                .text_color(theme.paint(Paint::Text))
                .child(body.to_owned()),
        )
}

fn absence(theme: &Theme, block: &SourceBlock) -> Div {
    surface::sunken(theme)
        .w_full()
        .px(space(Space::Room))
        .py(space(Space::Base))
        .child(
            text::dim(theme).child(
                block
                    .absence()
                    .unwrap_or("No source is available for this declaration."),
            ),
        )
}

fn remainder(total: usize, shown: usize, truncated: bool) -> String {
    let hidden = total.saturating_sub(shown);
    if truncated {
        return format!("{hidden} more lines retained, and the declaration continues past them");
    }
    format!("{hidden} more lines")
}

/// Returns the language tag builder shared with the project page.
pub(super) fn language_of(page: &Page) -> Language {
    page.language()
}
