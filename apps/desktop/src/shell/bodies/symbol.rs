//! The declaration page, plain: hero, one facts line, the lens bar, the
//! declaration's code, its docs, the relation list (the rose's narrow form),
//! and the Made of / Does ledgers as mark + name + one sentence rows.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf, Lens};
use crate::model::pages::{
    DeclRef, DocFragment, Member, PageKey, Receiver, Relation, SignatureText, SymbolPage, SymbolRef,
    TokenClass,
};
use crate::navigation::{Intent, Route, SymbolRoute, View};
use crate::shell::focus::{Act, Target};
use crate::shell::kit::{HoverIntent, gap_words, kind_of, quiet, shared_id, symbol_route, text};
use crate::shell::reader::Reader;
use facet::icons::{self, Icon, IconSize, KindSize};
use facet::tokens::ty;
use facet::{Density, Measure, Palette, Space};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, HighlightStyle, Hsla, InteractiveElement,
    InteractiveText, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, StyledText, div, px,
};
use std::ops::Range;
use std::rc::Rc;

pub(super) fn body(
    place: &Route,
    route: &SymbolRoute,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    hover: &mut HoverIntent,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let Some(symbol) = crate::runtime::store::route_symbol(place) else {
        return vec![Leaf::new(quiet("This page's address is not a declaration.", &ctx.measure, ctx.palette))];
    };
    let resource = store.symbol(&symbol);
    let name = symbol.identity().name().to_owned();
    let page = match shown(&resource) {
        Shown::Ready(page) => page.clone(),
        other => return not_ready(&other, &PageKey::Symbol(symbol), &name, ctx, cx),
    };
    let package = route.package.as_str().to_owned();
    let mut leaves = vec![hero(&page, ctx, cx), lens_bar(&page, ctx, cx)];
    match ctx.lens {
        Lens::Reference => {
            if let Some(code) = declaration(&page, ctx, cx) {
                leaves.push(Leaf::new(code));
            }
            if let Some(docs) = docs(&page, &package, ctx) {
                leaves.push(Leaf::new(docs));
            }
            leaves.push(Leaf::new(relations(&page, &package, ctx, hover, cx, true)));
            leaves.extend(ledgers(&page, &package, ctx, hover, cx));
        }
        Lens::Relations => leaves.push(Leaf::new(relations(&page, &package, ctx, hover, cx, false))),
        Lens::Usage => leaves.push(Leaf::new(usage(&page, ctx))),
        Lens::History => {
            let line = ctx.say("No release history is recorded for this declaration yet.");
            leaves.push(Leaf::new(quiet(line, &ctx.measure, ctx.palette)));
        }
    }
    leaves
}

fn hero(page: &SymbolPage, ctx: &mut Ctx<'_>, cx: &gpui::App) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let kind = kind_of(page.identity.kind);
    let name = ctx.say(page.identity.name.to_string());
    let lede = page
        .docs
        .iter()
        .find_map(|fragment| match fragment {
            DocFragment::Text(text) => text.split_terminator(['.', '\n']).map(str::trim).find(|line| !line.is_empty()),
            _ => None,
        })
        .map(|sentence| format!("{sentence}."));
    // Below 560 effective px the gem stands above the name at 40 px instead
    // of taking a third of the width beside it.
    let stacked = measure.effective() < 560.0;
    let gem_size = if stacked { 40.0 * measure.scale() } else { f32::from(measure.fluid(48.0, 64.0)) };
    let gap = measure.space(Space::Wide);
    let name_width = if stacked { measure.width() } else { measure.width() - px(gem_size) - gap };
    // The subject never ellipsizes: it wraps at identifier boundaries and
    // only steps its size down when one segment cannot fit.
    let (lines, role) = crate::shell::text_fit::fit_name(&name, ty::HERO, &measure, name_width.max(px(1.0)), cx);
    ctx.hero.extend(lines.iter().map(|line| SharedString::from(line.clone())));
    let mut words = div()
        .flex()
        .flex_col()
        .min_w(px(0.0))
        .gap(measure.space(Space::Tight))
        .child(crate::shell::text_fit::name_lines(&lines, role, palette.ink0));
    if let Some(lede) = lede {
        let lede = ctx.say(lede);
        words = words.child(text(ty::LEDE, &measure, palette.ink2).child(lede));
    }
    let key = crate::shell::kit::shared_key(&page.identity.coordinate);
    let gem = div()
        .id(shared_id(&page.identity.coordinate))
        .debug_selector(move || key)
        .child(facet::paint::gem(kind).size(gem_size));
    let top = if stacked {
        div().flex().flex_col().gap(measure.space(Space::Roomy)).child(gem).child(words)
    } else {
        div().flex().items_center().gap(gap).child(gem).child(words)
    };
    // One facts line: what it is and where, and your uses when known.
    let mut facts = vec![format!(
        "{} in {}",
        page.identity.kind_name(),
        where_is(&page.identity)
    )];
    if let (Some(path), Some(line)) = (&page.identity.path, page.identity.line) {
        facts.push(format!("{path}:{line}"));
    }
    let uses = page.references.known().map(|sites| sites.len());
    let mut line = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(measure.space(Space::Base))
        .set_text(ty::SMALL, &measure, palette.ink3);
    for (index, fact) in facts.into_iter().enumerate() {
        if index > 0 {
            line = line.child(dot(palette));
        }
        let fact = ctx.say(fact);
        line = line.child(fact);
    }
    if let Some(uses) = uses {
        let said = ctx.say(format!("{uses} uses"));
        line = line.child(dot(palette)).child(
            div()
                .flex()
                .gap(px(4.0))
                .child(div().text_color(palette.mint.base.hsla()).child(uses.to_string()))
                .child(said.replace(&format!("{uses} "), "")),
        );
    }
    Leaf::new(div().flex().flex_col().gap(measure.space(Space::Roomy)).child(top).child(line))
}

fn where_is(decl: &DeclRef) -> String {
    let identity = decl.coordinate.identity();
    let mut parts = Vec::new();
    if let Some(project) = identity.project() {
        parts.push(project.name().to_owned());
    }
    if let Some(path) = identity.path() {
        let stem = path.stem();
        if !stem.is_empty() && !matches!(stem, "lib" | "mod" | "main") {
            parts.push(stem.to_owned());
        }
    }
    parts.join("::")
}

fn dot(palette: &Palette) -> AnyElement {
    div().text_color(palette.ink4.hsla()).child("·").into_any_element()
}

trait SetText: Styled + Sized {
    fn set_text(self, role: facet::tokens::TypeRole, measure: &Measure, color: impl Into<Hsla>) -> Self;
}

impl<E: Styled> SetText for E {
    fn set_text(self, role: facet::tokens::TypeRole, measure: &Measure, color: impl Into<Hsla>) -> Self {
        use facet::Set as _;
        self.set(role, measure).text_color(color.into())
    }
}

fn lens_bar(page: &SymbolPage, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut bar = div()
        .flex()
        .items_end()
        .gap(measure.space(Space::Wide))
        .border_b_1()
        .border_color(palette.line1.hsla());
    let mut tabs: Vec<(SharedString, Option<Lens>)> = Lens::ALL.iter().map(|lens| (lens.name().into(), Some(*lens))).collect();
    tabs.push(("Source".into(), None));
    for (label, lens) in tabs {
        let on = lens == Some(ctx.lens);
        let id: SharedString = format!("lens-{label}").into();
        let act: Act = match lens {
            Some(lens) => {
                let weak = cx.weak_entity();
                Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |reader, cx| reader.set_lens(lens, cx));
                })
            }
            None => {
                // The code is a view of this declaration, not a place: it
                // replaces the entry (Back still leaves the declaration).
                let links = ctx.links.clone();
                Rc::new(move |_, cx| links.dispatch(Intent::SetView(View::Code), cx))
            }
        };
        ctx.targets.push(Target {
            id: id.clone(),
            label: label.clone(),
            act: Rc::clone(&act),
            peek: None,
            source: None,
        });
        let said = ctx.say(label.clone());
        let tab = div()
            .id(id.clone())
            .relative()
            .pb(measure.space(Space::Base))
            .child(text(ty::ROW, &measure, if on { palette.ink0 } else { palette.ink3 }).child(said))
            .children(on.then(|| div().absolute().left_0().right_0().bottom(px(-1.0)).h(px(2.0)).bg(palette.mint.base)))
            .on_click(move |_: &ClickEvent, window, cx| act(window, cx));
        bar = bar.child(ctx.targets.track(id, tab));
    }
    Leaf::new(bar)
}

/// The declaration's own text: the bounded excerpt when captured, else the
/// signature.
fn declaration(page: &SymbolPage, ctx: &mut Ctx<'_>, cx: &gpui::App) -> Option<AnyElement> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let (code, runs) = if let Some(excerpt) = page.site.excerpt.known() {
        let code = excerpt.text.to_string();
        let runs = keyword_runs(&code, palette);
        (code, runs)
    } else if let Some(signature) = page.signature.known() {
        (signature.text.to_string(), signature_runs(signature, page, palette))
    } else {
        let gap = page.signature.gap()?;
        return Some(quiet(ctx.say(gap_words(gap)), &measure, palette).into_any_element());
    };
    ctx.say(code.clone());
    // Never clipped: each line soft-wraps at token boundaries.
    let pad = measure.space(Space::Gutter);
    let role = measure.role(ty::CODE);
    let columns = crate::shell::text_fit::columns(measure.width() - pad * 2.0, &role, cx);
    let lines = crate::shell::text_fit::wrap_code(&code, &runs, columns);
    let body = div().flex().flex_col().children(lines.into_iter().map(|line| {
        text(ty::CODE, &measure, palette.ink1)
            .whitespace_nowrap()
            .child(StyledText::new(line.text).with_highlights(line.runs))
    }));
    Some(
        div()
            .bg(palette.table.hsla())
            .px(pad)
            .py(measure.space(Space::Roomy))
            .child(body)
            .into_any_element(),
    )
}

/// Highlight runs for a classified signature, from `palette.syntax`.
pub(crate) fn signature_runs(signature: &SignatureText, page: &SymbolPage, palette: &Palette) -> Vec<(Range<usize>, HighlightStyle)> {
    let syntax = &palette.syntax;
    let name_color: Hsla = match page.identity.family {
        crate::model::pages::KindFamily::Callable => syntax.function.into(),
        crate::model::pages::KindFamily::Contract => syntax.contract.into(),
        _ => syntax.type_name.into(),
    };
    signature
        .tokens
        .iter()
        .filter_map(|token| {
            let color: Hsla = match token.class {
                TokenClass::Keyword => syntax.keyword.into(),
                TokenClass::Name => name_color,
                TokenClass::Type => syntax.type_name.into(),
                TokenClass::Binding => syntax.parameter.into(),
                TokenClass::Lifetime => syntax.macro_name.into(),
                TokenClass::Punctuation => syntax.punctuation.into(),
                TokenClass::Literal => syntax.number.into(),
                TokenClass::Text => return None,
            };
            Some((
                token.span.range(),
                HighlightStyle {
                    color: Some(color),
                    ..HighlightStyle::default()
                },
            ))
        })
        .collect()
}

/// Keyword runs for unclassified code: the reserved words every language in
/// the set spells the same way, nothing else (real highlighting is the
/// highlighter's).
fn keyword_runs(code: &str, palette: &Palette) -> Vec<(Range<usize>, HighlightStyle)> {
    const WORDS: [&str; 24] = [
        "pub", "fn", "struct", "enum", "trait", "impl", "type", "const", "static", "mod", "use", "let",
        "class", "interface", "def", "func", "return", "async", "self", "Self", "where", "for", "in", "mut",
    ];
    let keyword: Hsla = palette.syntax.keyword.into();
    let comment: Hsla = palette.syntax.comment.into();
    let mut runs = Vec::new();
    let mut offset = 0;
    for line in code.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') {
            runs.push((
                offset..offset + line.trim_end().len(),
                HighlightStyle {
                    color: Some(comment),
                    ..HighlightStyle::default()
                },
            ));
        } else {
            let mut start = None;
            for (index, character) in line.char_indices().chain(std::iter::once((line.len(), ' '))) {
                let word = character.is_alphanumeric() || character == '_';
                match (word, start) {
                    (true, None) => start = Some(index),
                    (false, Some(begin)) => {
                        if WORDS.contains(&&line[begin..index]) {
                            runs.push((
                                offset + begin..offset + index,
                                HighlightStyle {
                                    color: Some(keyword),
                                    ..HighlightStyle::default()
                                },
                            ));
                        }
                        start = None;
                    }
                    _ => {}
                }
            }
        }
        offset += line.len();
    }
    runs
}

/// Docs as one styled text per paragraph; links are clickable ranges.
fn docs(page: &SymbolPage, package: &str, ctx: &mut Ctx<'_>) -> Option<AnyElement> {
    if page.docs.is_empty() {
        return None;
    }
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut paragraphs: Vec<(String, Vec<(Range<usize>, HighlightStyle)>, Vec<(Range<usize>, SymbolRef)>)> = vec![Default::default()];
    for fragment in page.docs.iter() {
        let (text, highlights, links) = paragraphs.last_mut()?;
        match fragment {
            DocFragment::Text(prose) => {
                for (index, part) in prose.split("\n\n").enumerate() {
                    if index > 0 {
                        paragraphs.push(Default::default());
                    }
                    let (text, _, _) = paragraphs.last_mut()?;
                    text.push_str(&part.replace('\n', " "));
                }
            }
            DocFragment::Code(code) => {
                let start = text.len();
                text.push_str(code);
                highlights.push((
                    start..text.len(),
                    HighlightStyle {
                        color: Some(palette.ink0.into()),
                        ..HighlightStyle::default()
                    },
                ));
            }
            DocFragment::Link { label, coordinate, .. } => {
                let start = text.len();
                text.push_str(label);
                highlights.push((
                    start..text.len(),
                    HighlightStyle {
                        color: Some(palette.peri.base.into()),
                        ..HighlightStyle::default()
                    },
                ));
                if let Some(target) = coordinate {
                    links.push((start..text.len(), target.clone()));
                }
            }
            DocFragment::Break => paragraphs.push(Default::default()),
        }
    }
    let mut column = div().flex().flex_col().gap(measure.space(Space::Roomy)).max_w(px(680.0 * measure.scale()));
    // The hero already says the first sentence: the body starts after it.
    let lede = page.docs.iter().find_map(|fragment| match fragment {
        DocFragment::Text(text) => text.split_terminator(['.', '\n']).map(str::trim).find(|line| !line.is_empty()).map(str::to_owned),
        _ => None,
    });
    for (index, (paragraph, highlights, links)) in paragraphs.into_iter().enumerate() {
        let mut shift = paragraph.len() - paragraph.trim_start().len();
        let mut trimmed = paragraph.trim().to_owned();
        if index == 0
            && let Some(lede) = &lede
            && let Some(rest) = trimmed.strip_prefix(lede.as_str())
        {
            let rest = rest.strip_prefix('.').unwrap_or(rest);
            let rest_trimmed = rest.trim_start();
            shift += trimmed.len() - rest_trimmed.len();
            trimmed = rest_trimmed.to_owned();
        }
        if trimmed.is_empty() {
            continue;
        }
        let said = ctx.say(trimmed.clone());
        let clamp = |range: &Range<usize>| {
            range.start.saturating_sub(shift).min(trimmed.len())..range.end.saturating_sub(shift).min(trimmed.len())
        };
        let highlights = highlights.iter().map(|(range, style)| (clamp(range), *style)).collect::<Vec<_>>();
        let ranges = links.iter().map(|(range, _)| clamp(range)).collect::<Vec<_>>();
        let targets = links.into_iter().map(|(_, target)| target).collect::<Vec<_>>();
        let dispatch = ctx.links.clone();
        let package = package.to_owned();
        let styled = StyledText::new(said).with_highlights(highlights);
        let interactive = InteractiveText::new(SharedString::from(format!("doc-{index}")), styled).on_click(ranges, move |which, _, cx| {
            if let Some(route) = targets.get(which).and_then(|target| symbol_route(&package, target)) {
                dispatch.dispatch(Intent::Navigate(route), cx);
            }
        });
        column = column.child(text(ty::PROSE, &measure, palette.ink1).child(interactive));
    }
    Some(column.into_any_element())
}

/// The rose in its narrow form: one line per direction.
fn relations(
    page: &SymbolPage,
    package: &str,
    ctx: &mut Ctx<'_>,
    hover: &mut HoverIntent,
    cx: &mut Context<Reader>,
    brief: bool,
) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let _ = hover;
    let mut column = div().flex().flex_col().gap(measure.space(Space::Base));
    let directions = [
        ("is", &page.rose.up),
        ("made of", &page.rose.down),
        ("from", &page.rose.left),
        ("to", &page.rose.right),
    ];
    let mut said_gap = false;
    for (label, relations) in directions {
        match relations.known() {
            Some(list) if !list.is_empty() => {
                let limit = if brief { 6 } else { usize::MAX };
                let mut names = div().flex().flex_wrap().gap(measure.space(Space::Base)).flex_1().min_w(px(0.0));
                for relation in list.iter().take(limit) {
                    names = names.child(relation_link(relation, package, ctx, cx));
                }
                if list.len() > limit {
                    let more = ctx.say(format!("and {} more", list.len() - limit));
                    names = names.child(text(ty::SMALL, &measure, palette.ink3).child(more));
                }
                let label = ctx.say(label);
                column = column.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(measure.space(Space::Roomy))
                        .child(text(ty::CAPTION, &measure, palette.ink3).w(px(72.0 * measure.scale())).flex_none().child(label))
                        .child(names),
                );
            }
            Some(_) => {}
            None => {
                if !said_gap && let Some(gap) = relations.gap() {
                    said_gap = true;
                    let words = ctx.say(gap_words(gap));
                    column = column.child(quiet(words, &measure, palette));
                }
            }
        }
    }
    column.into_any_element()
}

fn relation_link(relation: &Relation, package: &str, ctx: &mut Ctx<'_>, _cx: &mut Context<Reader>) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let name = ctx.say(relation.decl.name.to_string());
    let target = relation.decl.coordinate.clone();
    let route = symbol_route(package, &target);
    let id: SharedString = format!("rel-{}", target.as_str()).into();
    let links = ctx.links.clone();
    let act: Act = Rc::new(move |_, cx| {
        if let Some(route) = route.clone() {
            links.dispatch(Intent::Navigate(route), cx);
        }
    });
    ctx.targets.push(Target {
        id: id.clone(),
        label: name.clone(),
        act: Rc::clone(&act),
        peek: Some(PageKey::Symbol(target.clone())),
        source: Some(target),
    });
    ctx.targets
        .track(
            id.clone(),
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(5.0 * measure.scale()))
                .child(crate::shell::kit::kind_mark(kind_of(relation.decl.kind), KindSize::Sm, &measure, palette))
                .child(text(ty::MONO_ROW, &measure, palette.ink1).hover(|style| style.text_color(palette.ink0.hsla())).child(name))
                .on_click(move |_: &ClickEvent, window, cx| act(window, cx)),
        )
        .into_any_element()
}

fn usage(page: &SymbolPage, ctx: &mut Ctx<'_>) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    match page.references.known() {
        Some(sites) if !sites.is_empty() => {
            let mut column = div().flex().flex_col().gap(measure.space(Space::Snug));
            for site in sites.iter().take(200) {
                let name = ctx.say(site.site.name.to_string());
                let place = site
                    .span
                    .known()
                    .map(|span| format!("{} · bytes {}–{}", span.file, span.bytes.start, span.bytes.end))
                    .unwrap_or_default();
                column = column.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(measure.space(Space::Base))
                        .h(measure.row())
                        .child(crate::shell::kit::kind_mark(kind_of(site.site.kind), KindSize::Sm, &measure, palette))
                        .child(text(ty::MONO_ROW, &measure, palette.ink1).child(name))
                        .child(text(ty::SMALL, &measure, palette.ink3).child(place)),
                );
            }
            column.into_any_element()
        }
        Some(_) => quiet(ctx.say("Nothing uses this yet."), &measure, palette).into_any_element(),
        None => {
            let words = page.references.gap().map(gap_words).unwrap_or_default();
            quiet(ctx.say(words), &measure, palette).into_any_element()
        }
    }
}

fn ledgers(
    page: &SymbolPage,
    package: &str,
    ctx: &mut Ctx<'_>,
    hover: &mut HoverIntent,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let Some(members) = page.members.known() else {
        return page
            .members
            .gap()
            .map(|gap| {
                let words = ctx.say(gap_words(gap));
                Leaf::new(quiet(words, &measure, palette))
            })
            .into_iter()
            .collect();
    };
    let mut leaves = Vec::new();
    if !members.made_of.is_empty() {
        let heading = ctx.say("Made of");
        let mut column = div().flex().flex_col().child(section_head(heading, &measure, palette));
        for member in members.made_of.iter() {
            column = column.child(member_row(member, package, ctx, hover, cx));
        }
        leaves.push(Leaf::new(column));
    }
    if !members.does.is_empty() {
        let heading = ctx.say("Does");
        let mut column = div().flex().flex_col().child(section_head(heading, &measure, palette));
        for group in members.does.iter() {
            let label = ctx.say(receiver_words(group.receiver));
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Snug))
                    .pt(measure.space(Space::Roomy))
                    .pb(measure.space(Space::Tight))
                    .child(icons::mod_mark(receiver_mark(group.receiver), 12.0 * measure.scale(), palette))
                    .child(text(ty::SMALL, &measure, palette.ink3).child(label)),
            );
            for member in group.members.iter() {
                column = column.child(member_row(member, package, ctx, hover, cx));
            }
        }
        leaves.push(Leaf::new(column));
    }
    if !members.other.is_empty() {
        let heading = ctx.say("Also here");
        let mut column = div().flex().flex_col().child(section_head(heading, &measure, palette));
        for member in members.other.iter() {
            column = column.child(member_row(member, package, ctx, hover, cx));
        }
        leaves.push(Leaf::new(column));
    }
    leaves
}

fn section_head(title: SharedString, measure: &Measure, palette: &Palette) -> AnyElement {
    text(ty::TITLE, measure, palette.ink0)
        .pt(measure.space(Space::Wide))
        .pb(measure.space(Space::Base))
        .child(title)
        .into_any_element()
}

const fn receiver_words(receiver: Receiver) -> &'static str {
    match receiver {
        Receiver::Reads => "reads",
        Receiver::Changes => "changes",
        Receiver::Consumes => "consumes",
        Receiver::Makes => "makes",
        Receiver::Unknown => "other",
    }
}

const fn receiver_mark(receiver: Receiver) -> icons::Mod {
    match receiver {
        Receiver::Reads => icons::Mod::Reads,
        Receiver::Changes => icons::Mod::Changes,
        Receiver::Consumes => icons::Mod::Consumes,
        Receiver::Makes | Receiver::Unknown => icons::Mod::Makes,
    }
}

/// One ledger row: mark, name with the rest of its signature, one sentence.
fn member_row(
    member: &Member,
    package: &str,
    ctx: &mut Ctx<'_>,
    _hover: &mut HoverIntent,
    cx: &mut Context<Reader>,
) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let symbol = member.decl.coordinate.clone();
    let id: SharedString = format!("m-{}", symbol.as_str()).into();
    let name = member.decl.name.to_string();
    // The signature from the name on, so a variant reads `Typed(A, B)` and a
    // method `as_str(self) -> &'static str`.
    let shape = member
        .signature
        .known()
        .and_then(|signature| {
            let text = signature.text.as_ref();
            text.find(name.as_str()).map(|at| text[at..].trim().to_owned())
        })
        .unwrap_or_else(|| name.clone());
    let shape = ctx.say(shape);
    let name_len = name.len().min(shape.len());
    let route = symbol_route(package, &symbol);
    let links = ctx.links.clone();
    let act: Act = Rc::new(move |_, cx| {
        if let Some(route) = route.clone() {
            links.dispatch(Intent::Navigate(route), cx);
        }
    });
    ctx.targets.push(Target {
        id: id.clone(),
        label: name.clone().into(),
        act: Rc::clone(&act),
        peek: Some(PageKey::Symbol(symbol.clone())),
        source: Some(symbol.clone()),
    });
    let summary = (measure.density() != Density::Dense)
        .then(|| member.summary.clone())
        .flatten()
        .map(|summary| ctx.say(summary.to_string()));
    let focused = ctx.targets.is_focused(&id);
    // The row's trailing zone: quiet at rest, its facts when the keyboard
    // stands on it or ⌥ x-rays the page (§6.2 rule 2).
    let facts = (focused || ctx.reveal.xray).then(|| {
        let mut facts = vec![member.decl.kind_name().to_owned()];
        if let (Some(path), Some(line)) = (&member.decl.path, member.decl.line) {
            facts.push(format!("{path}:{line}"));
        }
        ctx.say(facts.join(" · "))
    });
    let warm = PageKey::Symbol(symbol);
    let styled = StyledText::new(shape.clone()).with_highlights([
        (
            0..name_len,
            HighlightStyle {
                color: Some(palette.ink0.into()),
                font_weight: Some(FontWeight(600.0)),
                ..HighlightStyle::default()
            },
        ),
        (
            name_len..shape.len(),
            HighlightStyle {
                color: Some(palette.ink2.into()),
                ..HighlightStyle::default()
            },
        ),
    ]);
    let row = div()
        .id(id.clone())
        .flex()
        .items_center()
        .gap(measure.space(Space::Roomy))
        .min_h(measure.row() + measure.space(Space::Base))
        .px(measure.space(Space::Base))
        .hover(|style| style.bg(palette.tint))
        .when_focused(focused, palette)
        .child(crate::shell::kit::kind_mark(kind_of(member.decl.kind), KindSize::Sm, &measure, palette))
        .child(text(ty::MONO_ROW, &measure, palette.ink1).flex_none().max_w(px(520.0 * measure.scale())).overflow_hidden().whitespace_nowrap().text_ellipsis().child(styled))
        .children(summary.map(|summary| {
            text(ty::CAPTION, &measure, palette.ink3)
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(summary)
        }))
        .children(facts.map(|facts| {
            text(ty::MONO_SMALL, &measure, palette.ink3)
                .flex_none()
                .ml_auto()
                .pl(measure.space(Space::Roomy))
                .child(facts)
        }))
        .on_click(move |_: &ClickEvent, window, cx| act(window, cx))
        .on_hover(cx.listener(move |reader, hovered: &bool, _, cx| reader.hover_link(warm.clone(), *hovered, cx)));
    ctx.targets.track(id, row).into_any_element()
}

trait WhenFocused: Styled + Sized {
    fn when_focused(self, focused: bool, palette: &Palette) -> Self {
        if focused { self.bg(palette.tint) } else { self }
    }
}

impl<E: Styled> WhenFocused for E {}

#[allow(dead_code)]
fn _icon(_: Icon, _: IconSize) {}
