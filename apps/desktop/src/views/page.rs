//! One declaration page: what it is, what it means, what it holds, who uses it.
//! Every type name in the signature and every link in the prose is navigable.
//! Absences are stated — a missing member group says which kind of missing.
//!
//! The order is the order a reader asks, and it is docs.rs's order because
//! docs.rs got it right: the trail and the name, the signature as the shape,
//! the documentation as the meaning, then the parts — variants, fields,
//! methods, each with its own signature and first sentence — and finally the
//! relations. The source text is not a section. It is one small `src` control
//! in the header, the way it is on docs.rs, and it opens as a sheet over the
//! page with every identifier highlighted and linked, so the page stays about
//! what the declaration *means* and the file is one keystroke away.
//!
//! Nothing is a card; the page is one column of sections separated by
//! hairlines, which is what lets a dense declaration and a sparse one look
//! like the same document. Groups fold, and beyond a budget they offer the
//! rest rather than silently dropping it.

use super::keys;
use super::workspace::Workspace;
use crate::presentation::crumb::{self, Crumb};
use crate::store::document::Target;
use crate::theme::Theme;
use crate::theme::kind::group_title;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use crate::ui::icon::{self, Logo};
use crate::ui::tip::{Tip, Tipped as _};
use crate::ui::{button, glyph, prose, specimen, text};
use backend_present::{
    Identity, IdentityKey, Member, MemberGroup, Page, RelationDirection, RelationGroup,
    RelationLabel, Source,
};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px,
};

/// How many members are drawn before a group offers to show the rest.
const MEMBER_BUDGET: usize = 40;

/// How many relations are drawn before a group offers to show the rest.
const RELATION_BUDGET: usize = 24;

/// Character budget for a member's inline signature preview.
const MEMBER_PREVIEW: usize = 110;

/// One top-level region of a declaration page, in reading order.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Region {
    /// Trail, name, kind, language, and the source and copy controls.
    Header,
    /// One fault the assembly recorded while gathering the page.
    Note(usize),
    /// The tokenized signature specimen and the types it names.
    Signature,
    /// The producer's documentation.
    Documentation,
    /// One member group, addressed by its position among the groups.
    Members(usize),
    /// One relation group, addressed by its position among the groups.
    Relations(usize),
}

/// Returns every region one page has, in the order the reader draws them.
fn regions(page: &Page) -> Vec<Region> {
    let mut regions = vec![Region::Header];
    regions.extend((0..page.notes().len()).map(Region::Note));
    if page.signature().is_some() {
        regions.push(Region::Signature);
    }
    if !page.prose().is_empty() && !prose_is_tautological(page) {
        regions.push(Region::Documentation);
    }
    regions.extend(member_order(page).into_iter().map(Region::Members));
    regions.extend((0..page.relations().len()).map(Region::Relations));
    regions
}

/// Returns the member groups in reading order: parts before behaviour.
///
/// docs.rs lists a type's variants and fields before its methods, because
/// what a value *is* comes before what it can do. The shared assembly groups
/// by wire tag; this page reorders the groups, never their contents.
fn member_order(page: &Page) -> Vec<usize> {
    let mut order: Vec<usize> = (0..page.members().len()).collect();
    order.sort_by_key(|at| {
        page.members()
            .get(*at)
            .map_or(u8::MAX, |group| part_rank(group.kind()))
    });
    order
}

const fn part_rank(kind: backend_library::DeclarationKind) -> u8 {
    use backend_library::DeclarationKind as K;
    match kind {
        K::Variant => 0,
        K::Field => 1,
        K::Property => 2,
        K::Constant => 3,
        K::Constructor => 4,
        K::Method => 5,
        K::Function => 6,
        K::Type => 7,
        K::Struct | K::Enum | K::Union | K::Class => 8,
        K::Interface | K::Trait => 9,
        K::Module => 10,
        K::Macro | K::Variable | K::Import | K::Unknown => 11,
    }
}

/// Returns the keys of every member on the page, so relations can skip them.
fn member_keys(page: &Page) -> Vec<backend_library::SymbolKey> {
    page.members()
        .iter()
        .flat_map(MemberGroup::members)
        .filter_map(|member| symbol_of(member.identity()))
        .collect()
}

impl Workspace {
    /// Returns each region of the declaration page as its own element.
    pub(super) fn declaration_page(
        &mut self,
        theme: &Theme,
        page: &Page,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        regions(page)
            .into_iter()
            .map(|region| self.region(theme, page, &region, cx))
            .collect()
    }

    fn region(
        &mut self,
        theme: &Theme,
        page: &Page,
        region: &Region,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match region {
            Region::Header => self.page_header(theme, page, cx).into_any_element(),
            Region::Note(at) => match page.notes().get(*at) {
                Some(note) => Self::note_region(theme, *at, note, cx),
                None => div().into_any_element(),
            },
            Region::Signature => match page.signature() {
                Some(signature) => self
                    .signature_section(theme, signature, cx)
                    .into_any_element(),
                None => div().into_any_element(),
            },
            Region::Documentation => self.prose_section(theme, page).into_any_element(),
            Region::Members(at) => match page.members().get(*at) {
                Some(group) => self.member_group(theme, group, cx),
                None => div().into_any_element(),
            },
            Region::Relations(at) => match page.relations().get(*at) {
                Some(group) => self.relation_group(theme, page, group, cx),
                None => div().into_any_element(),
            },
        }
    }

    fn note_region(
        theme: &Theme,
        at: usize,
        note: &backend_present::Fault,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let actions = Self::affordances(theme, &format!("page-note-{at}"), note, "", cx);
        crate::ui::fault::block(theme, note, actions).into_any_element()
    }

    fn page_header(&self, theme: &Theme, page: &Page, cx: &mut Context<Self>) -> impl IntoElement {
        let identity = page.identity().clone();
        let containers = self.containers(page, cx);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(Self::trail(theme, &identity, &containers, cx))
                    .child(div().flex_1())
                    .child(Self::header_controls(theme, page, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Base))
                    .child(glyph::kind_tile(theme, page.kind(), true))
                    .child(
                        text::heading(theme, TypeScale::Title)
                            .flex_none()
                            .child(identity.name().to_owned()),
                    )
                    .child(Self::kind_language(theme, page)),
            )
    }

    /// Returns the kind word and the language logo beside the name.
    fn kind_language(theme: &Theme, page: &Page) -> Div {
        let language = page.language();
        let ink = theme.on_plane(crate::theme::language::hue(language));
        div()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .pt(px(6.0))
            .child(text::dim(theme).child(glyph::kind_label(page.kind())))
            .when_some(Logo::of(language), |head, logo| {
                head.child(
                    div()
                        .id("page-language")
                        .flex_none()
                        .child(icon::logo(logo, 14.0, ink))
                        .tip(Tip::new(crate::theme::language::label(language))
                            .detail("The language this declaration is written in.")),
                )
            })
    }

    /// Returns the `src` control, the key tag, and the copy control.
    fn header_controls(theme: &Theme, page: &Page, cx: &mut Context<Self>) -> Div {
        let identity = page.identity().clone();
        let coordinate = identity.coordinate().as_str().to_owned();
        let site = page.source().site().map(|site| {
            format!("{}:{}", site.path().as_str(), site.line().get())
        });
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(space(Space::Snug))
            .when_some(site, |controls, site| {
                controls.child(
                    source_button(theme)
                        .tip(Tip::new("Show source")
                            .detail("The captured text, highlighted, with every known name linked.")
                            .key(keys::OPEN_SOURCE)
                            .value(site))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_source(cx))),
                )
            })
            .child(Self::key_tag(theme, &identity, cx))
            .child(
                button::icon_button(theme, "copy-identity", icon::Icon::Copy)
                    .tip(Tip::new("Copy coordinate")
                        .key(keys::COPY_IDENTITY)
                        .value(coordinate.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy("Coordinate copied", coordinate.clone(), cx);
                    })),
            )
    }

    fn key_tag(theme: &Theme, identity: &Identity, cx: &mut Context<Self>) -> AnyElement {
        let Some((tag, full)) = identity.key().tag().zip(identity.key().encoded()) else {
            return div().into_any_element();
        };
        let copied = full.clone();
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
            .child(tag.to_string())
            .tip(Tip::new("Copy stable key")
                .detail("The key an agent or the CLI addresses this declaration by.")
                .key(keys::COPY_KEY)
                .value(full))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.copy("Key copied", copied.clone(), cx);
            }))
            .into_any_element()
    }

    /// Returns the declarations that contain this page's, outermost first.
    ///
    /// The coordinate alone knows the file and the name; the index knows
    /// that `enumeration` sits inside `ArgumentKind`, and a trail that skips
    /// the type is a trail that lies about where the reader is.
    fn containers(&self, page: &Page, cx: &Context<Self>) -> Vec<(backend_library::SymbolKey, String)> {
        let IdentityKey::Symbol(symbol) = page.identity().key() else {
            return Vec::new();
        };
        let index = self.index.read(cx);
        index
            .project_of(symbol)
            .map(|project| {
                project
                    .ancestors(symbol)
                    .into_iter()
                    .filter(|entry| !entry.is_file_module())
                    .map(|entry| (entry.symbol(), entry.name().to_owned()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn trail(
        theme: &Theme,
        identity: &Identity,
        containers: &[(backend_library::SymbolKey, String)],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut crumbs = crumb::trail(identity);
        let leaf = crumbs.len().saturating_sub(1);
        for (symbol, name) in containers.iter().rev() {
            crumbs.insert(leaf, Crumb::Container {
                label: name.clone(),
                symbol: *symbol,
            });
        }
        let coordinate = identity.coordinate().as_str().to_owned();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space(Space::Tight))
            .children(crumbs.into_iter().enumerate().map(|(at, step)| {
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
                    .child(Self::crumb(theme, at, &step, &coordinate, cx))
                    .into_any_element()
            }))
    }

    fn crumb(
        theme: &Theme,
        at: usize,
        step: &Crumb,
        coordinate: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = step.label().to_owned();
        let navigable = step.is_navigable();
        let destination = crumb_destination(step, coordinate);
        let body = text::identity_text(theme, TypeScale::Small)
            .child(text::elide(&label, 42).to_string());
        if !navigable {
            return div()
                .child(body.text_color(theme.paint(Paint::TextDim)))
                .into_any_element();
        }
        div()
            .id(ElementId::Name(SharedString::from(format!("crumb-{at}"))))
            .cursor_pointer()
            .rounded(radius(Radius::Hair))
            .px(px(2.0))
            .hover(|style| style.bg(theme.paint(Paint::GiltWash)))
            .child(body)
            .tip(crumb_tip(step))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_crumb(&destination, cx);
            }))
            .into_any_element()
    }

    fn signature_section(
        &mut self,
        theme: &Theme,
        signature: &backend_present::Signature,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let document = self.document.clone();
        let types = self.named_types(theme, signature, cx);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(specimen::signature_block(
                theme,
                signature,
                "page-signature",
                move |symbol, _, cx| {
                    open_symbol(&document, symbol, cx);
                },
            ))
            .when_some(types, ParentElement::child)
    }

    /// Returns the row of types a signature names, each one a door.
    ///
    /// The signature already underlines them; this row states them once,
    /// with their kinds, so a reader can see at a glance which types a
    /// function speaks in without parsing the specimen.
    fn named_types(
        &self,
        theme: &Theme,
        signature: &backend_present::Signature,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let mut seen: Vec<backend_library::SymbolKey> = Vec::new();
        let named: Vec<(backend_library::SymbolKey, String, Option<backend_library::DeclarationKind>)> = {
            let index = self.index.read(cx);
            signature
                .tokens()
                .iter()
                .filter_map(|token| {
                    let target = token.target()?;
                    let IdentityKey::Symbol(symbol) = target.key() else {
                        return None;
                    };
                    if seen.contains(&symbol) {
                        return None;
                    }
                    seen.push(symbol);
                    let kind = index.entry(symbol).and_then(crate::store::index::Entry::kind);
                    Some((symbol, token.text().to_owned(), kind))
                })
                .collect()
        };
        let chips: Vec<AnyElement> = named
            .into_iter()
            .enumerate()
            .map(|(at, (symbol, name, kind))| Self::type_chip(theme, at, symbol, &name, kind, cx))
            .collect();
        if chips.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(space(Space::Tight))
                .child(text::faint(theme).flex_none().pr(px(2.0)).child("Types"))
                .children(chips),
        )
    }

    fn type_chip(
        theme: &Theme,
        at: usize,
        symbol: backend_library::SymbolKey,
        name: &str,
        kind: Option<backend_library::DeclarationKind>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(ElementId::Name(SharedString::from(format!("type-{at}"))))
            .flex()
            .flex_none()
            .items_center()
            .gap(space(Space::Tight))
            .pl(px(4.0))
            .pr(space(Space::Snug))
            .py(px(2.0))
            .rounded(radius(Radius::Hair))
            .bg(theme.paint(Paint::Hover))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Selected)))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                this.open_symbol(symbol, super::context::click_target(event), cx);
            }))
            .on_hover(cx.listener(move |this, entering: &bool, window, cx| {
                this.hover_member(symbol, *entering, window, cx);
            }))
            .child(glyph::kind_mark(theme, kind, 11.0))
            .child(
                text::label(theme)
                    .text_size(type_size(TypeScale::Small))
                    .font_family(theme.specimen())
                    .child(name.to_owned()),
            )
            .into_any_element()
    }

    fn prose_section(&mut self, theme: &Theme, page: &Page) -> impl IntoElement {
        let document = self.document.clone();
        prose::blocks(theme, page.prose(), "page", move |symbol, _, cx| {
            open_symbol(&document, symbol, cx);
        })
    }
}

/// Member and relation groups.
impl Workspace {
    fn member_group(
        &mut self,
        theme: &Theme,
        group: &MemberGroup,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = group_title(group.kind());
        let key = format!("members-{}", group.kind().wire_tag());
        let members = group.members();
        let folded = self.is_folded(&key);
        let shown = if self.is_unfurled(&key) {
            members.len()
        } else {
            members.len().min(MEMBER_BUDGET)
        };
        let body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .children(
                members
                    .iter()
                    .take(shown)
                    .enumerate()
                    .map(|(at, member)| Self::member_row(theme, &key, at, member, cx)),
            )
            .when(members.len() > shown, |body| {
                body.child(Self::show_more(theme, &key, members.len() - shown, "members", cx))
            })
            .into_any_element();
        let head = GroupHead {
            key: &key,
            title: &title,
            kind: Some(group.kind()),
            count: members.len(),
            folded,
        };
        Self::folding_section(theme, &head, body, cx)
    }

    /// Returns one member as docs.rs draws it: name and signature on one
    /// line, the first sentence of its documentation under them.
    fn member_row(
        theme: &Theme,
        group: &str,
        at: usize,
        member: &Member,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(symbol) = symbol_of(member.identity()) else {
            return div().into_any_element();
        };
        let summary = member
            .summary()
            .filter(|summary| !prose::tautological(summary, member.identity()))
            .map(ToOwned::to_owned);
        let preview = member
            .signature()
            .map(|signature| specimen::signature_line(theme, signature, MEMBER_PREVIEW));
        div()
            .id(ElementId::Name(SharedString::from(format!(
                "member-{group}-{at}"
            ))))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .py(space(Space::Tight))
            .px(space(Space::Snug))
            .rounded(radius(Radius::Small))
            .min_w(px(0.0))
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                this.open_symbol(symbol, super::context::click_target(event), cx);
            }))
            .on_hover(cx.listener(move |this, entering: &bool, window, cx| {
                this.hover_member(symbol, *entering, window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .min_w(px(0.0))
                    .child(glyph::kind_mark(theme, member.kind(), 12.0))
                    .child(
                        text::single_line(text::navigable(theme, TypeScale::Interface, false))
                            .flex_none()
                            .max_w(px(280.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child(member.identity().name().to_owned()),
                    )
                    .when_some(preview, |row, preview| row.child(preview_cell(theme).child(preview))),
            )
            .when_some(summary, |row, summary| {
                row.child(
                    text::single_line(text::dim(theme))
                        .pl(px(20.0))
                        .child(summary),
                )
            })
            .into_any_element()
    }

    fn relation_group(
        &mut self,
        theme: &Theme,
        page: &Page,
        group: &RelationGroup,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = group.label();
        let key = format!("relations-{}", label.as_str());
        let folded = self.is_folded(&key);
        let members = member_keys(page);
        let entries: Vec<&backend_present::Relation> = group
            .relations()
            .iter()
            .filter(|entry| !is_file_module(entry))
            .filter(|entry| symbol_of(entry.identity()).is_none_or(|key| !members.contains(&key)))
            .collect();
        if entries.is_empty() {
            return div().into_any_element();
        }
        let shown = if self.is_unfurled(&key) {
            entries.len()
        } else {
            entries.len().min(RELATION_BUDGET)
        };
        let body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(1.0))
            .children(
                entries
                    .iter()
                    .take(shown)
                    .enumerate()
                    .map(|(at, entry)| Self::relation_row(theme, label.as_str(), at, entry, cx)),
            )
            .when(entries.len() > shown, |body| {
                body.child(Self::show_more(theme, &key, entries.len() - shown, "related", cx))
            })
            .into_any_element();
        let title = relation_title(label);
        let head = GroupHead {
            key: &key,
            title: &title,
            kind: None,
            count: entries.len(),
            folded,
        };
        Self::folding_section(theme, &head, body, cx)
    }

    fn relation_row(
        theme: &Theme,
        group: &str,
        at: usize,
        entry: &backend_present::Relation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(symbol) = symbol_of(entry.identity()) else {
            return div().into_any_element();
        };
        let within = entry.identity().project().cloned();
        let trail = entry.identity().trail_within(within.as_ref());
        div()
            .id(ElementId::Name(SharedString::from(format!(
                "relation-{group}-{at}"
            ))))
            .w_full()
            .flex()
            .items_center()
            .gap(space(Space::Snug))
            .py(px(3.0))
            .px(space(Space::Snug))
            .rounded(radius(Radius::Small))
            .cursor_pointer()
            .hover(|style| style.bg(theme.paint(Paint::Hover)))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                this.open_symbol(symbol, super::context::click_target(event), cx);
            }))
            .on_hover(cx.listener(move |this, entering: &bool, window, cx| {
                this.hover_member(symbol, *entering, window, cx);
            }))
            .child(glyph::kind_mark(theme, entry.kind(), 12.0))
            .child(
                text::single_line(text::navigable(theme, TypeScale::Interface, false))
                    .flex_none()
                    .max_w(px(280.0))
                    .child(crate::presentation::project::display_name(entry.identity(), entry.kind())),
            )
            .child(
                text::single_line(text::faint(theme))
                    .flex_1()
                    .min_w(px(0.0))
                    .font_family(theme.specimen())
                    .child(trail),
            )
            .into_any_element()
    }

    fn show_more(
        theme: &Theme,
        key: &str,
        hidden: usize,
        noun: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let owned = key.to_owned();
        button::button(
            theme,
            format!("more-{key}"),
            &format!("Show {hidden} more {noun}"),
            button::Weight::Quiet,
        )
        .mt(space(Space::Tight))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.unfurl(&owned, cx);
        }))
        .into_any_element()
    }

    /// Returns a section whose header folds its body away.
    fn folding_section(
        theme: &Theme,
        head: &GroupHead<'_>,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let owned = head.key.to_owned();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(space(Space::Snug))
            .child(
                div()
                    .id(ElementId::Name(SharedString::from(format!("fold-{}", head.key))))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .cursor_pointer()
                    .child(chevron(theme, head.folded))
                    .when_some(head.kind, |row, kind| {
                        row.child(glyph::kind_mark(theme, Some(kind), 12.0))
                    })
                    .child(
                        text::label(theme)
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.paint(Paint::TextStrong))
                            .child(head.title.to_owned()),
                    )
                    .child(text::faint(theme).child(head.count.to_string()))
                    .child(
                        div()
                            .flex_1()
                            .h(hairline())
                            .bg(theme.paint(Paint::Hairline)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_fold(&owned, cx);
                    })),
            )
            .when(!head.folded, |section| section.child(body))
            .into_any_element()
    }

    /// Opens one source site in the configured external editor.
    pub(super) fn open_in_editor(&mut self, path: &str, line: u32, cx: &mut Context<Self>) {
        let project = self.engine.read(cx).project().to_path_buf();
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

    /// Raises or clears the hover card for one member row.
    pub(super) fn hover_member(
        &mut self,
        symbol: backend_library::SymbolKey,
        entering: bool,
        window: &gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if !entering {
            self.document
                .update(cx, super::super::store::document::DocumentStore::clear_hover);
            return;
        }
        let anchor = window.mouse_position();
        self.document.update(cx, |document, cx| {
            document.hover_over(symbol, anchor, cx);
        });
    }
}

/// What a folding group's header states.
struct GroupHead<'a> {
    key: &'a str,
    title: &'a str,
    kind: Option<backend_library::DeclarationKind>,
    count: usize,
    folded: bool,
}

/// Where following one crumb goes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Destination {
    /// Open one project's page.
    Project(String),
    /// Open the module a source file is.
    File(String),
    /// Open one declaration by its coordinate prefix.
    Symbol(String),
    /// Open one declaration by its key.
    Key(backend_library::SymbolKey),
}

/// Returns whether the page's whole prose is the producer's placeholder line.
fn prose_is_tautological(page: &Page) -> bool {
    prose::summary(page.prose())
        .is_some_and(|summary| prose::tautological(&summary, page.identity()))
        && page.prose().len() <= 2
}

/// Returns the source site of a page, when it has one.
pub(super) fn site_of(page: &Page) -> Option<(String, u32)> {
    match page.source() {
        Source::Captured { site, .. } | Source::Sited { site, .. } => {
            Some((site.path().as_str().to_owned(), site.line().get()))
        }
        Source::Absent { .. } => None,
    }
}

fn crumb_destination(step: &Crumb, coordinate: &str) -> Destination {
    match step {
        Crumb::Project { root, .. } => Destination::Project(root.clone()),
        Crumb::Path { path, .. } => Destination::File(path.clone()),
        Crumb::Symbol { depth, .. } => {
            let prefix = coordinate
                .split("::")
                .take(depth.saturating_add(2))
                .collect::<Vec<_>>()
                .join("::");
            Destination::Symbol(prefix)
        }
        Crumb::Container { symbol, .. } => Destination::Key(*symbol),
    }
}

fn crumb_tip(step: &Crumb) -> Tip {
    match step {
        Crumb::Project { root, .. } => Tip::new("Open project").value(root.clone()),
        Crumb::Path { path, .. } => Tip::new("Open module").value(path.clone()),
        Crumb::Symbol { label, .. } | Crumb::Container { label, .. } => {
            Tip::new("Open container").value(label.clone())
        }
    }
}

/// Returns whether a relation is the file module the page already sits in.
fn is_file_module(entry: &backend_present::Relation) -> bool {
    entry.kind() == Some(backend_library::DeclarationKind::Module)
        && entry.identity().trail().is_empty()
}

fn symbol_of(identity: &Identity) -> Option<backend_library::SymbolKey> {
    match identity.key() {
        IdentityKey::Symbol(symbol) => Some(symbol),
        IdentityKey::Package(_) | IdentityKey::Absent => None,
    }
}

fn open_symbol(
    document: &Entity<crate::store::document::DocumentStore>,
    symbol: backend_library::SymbolKey,
    cx: &mut gpui::App,
) {
    document.update(cx, |document, cx| {
        document.open_symbol(symbol, Target::Child, cx);
    });
}

/// Returns the words one relation group is headed by.
fn relation_title(label: RelationLabel) -> String {
    match label {
        RelationLabel::Typed(_, RelationDirection::Incoming) => {
            format!("Used by · {}", label.as_str())
        }
        RelationLabel::Typed(_, RelationDirection::Outgoing) => {
            format!("Uses · {}", label.as_str())
        }
        RelationLabel::Neighbourhood | RelationLabel::Related => title_case(label.as_str()),
    }
}

/// Returns the small specimen-face control that opens the source sheet.
fn source_button(theme: &Theme) -> gpui::Stateful<Div> {
    div()
        .id("page-source")
        .flex()
        .flex_none()
        .items_center()
        .gap(px(3.0))
        .px(space(Space::Snug))
        .py(px(2.0))
        .rounded(radius(Radius::Hair))
        .border(hairline())
        .border_color(theme.paint(Paint::Hairline))
        .font_family(theme.specimen())
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(Paint::TextDim))
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(theme.paint(Paint::Hover))
                .text_color(theme.paint(Paint::TextStrong))
        })
        .child(icon::sized(theme, icon::Icon::Code, 11.0, Paint::TextDim))
        .child("src")
}

fn preview_cell(theme: &Theme) -> Div {
    div()
        .flex_1()
        .min_w(px(0.0))
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
        .font_family(theme.specimen())
        .text_size(type_size(TypeScale::Small))
        .text_color(theme.paint(Paint::TextDim))
}

fn chevron(theme: &Theme, folded: bool) -> Div {
    div()
        .flex_none()
        .w(px(12.0))
        .flex()
        .items_center()
        .justify_center()
        .child(icon::sized(
            theme,
            if folded {
                icon::Icon::ChevronRight
            } else {
                icon::Icon::ChevronDown
            },
            11.0,
            Paint::TextFaint,
        ))
}

fn title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (at, letter) in text.chars().enumerate() {
        if at == 0 {
            out.extend(letter.to_uppercase());
        } else {
            out.push(letter);
        }
    }
    out
}
