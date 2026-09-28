//! The declaration page, drawn (DESIGN.md direction A, `facet::anatomy::page`):
//! the gem on a spine, the name, the author's lede when there is one, the
//! specimen (a choice is a fork, a record a bracket), then its sections in
//! reading order, each a mark on the spine with its relations running out
//! to the margins. One column; no tabs, no narration, no facts row (the
//! chrome already says the kind, the path and the file).
//!
//! The page is compiled from the index's page for the declaration, the
//! pinned world's anatomy when it knows the declaration, and companion
//! pages (a Go named type's constants): `source` reads them into facet's
//! language-neutral `Source`; `facet::anatomy::plan::compile` is pure.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf};
use crate::model::pages::{DocFragment, PageKey, SymbolPage, SymbolRef};
use crate::navigation::{Intent, Route, SymbolRoute};
use crate::shell::kit::{HoverIntent, kind_of, shared_id, symbol_route, text};
use crate::shell::reader::Reader;
use facet::anatomy::page::{Anchors, Geometry};
use facet::anatomy::plan::SectionId;
use facet::tokens::{scale, ty};
use facet::Space;
use gpui::{
    AnyElement, Context, FontStyle, FontWeight, HighlightStyle, InteractiveElement, InteractiveText, IntoElement,
    ParentElement, SharedString, Styled, StyledText, div, px,
};
use std::ops::Range;

mod companions;
#[cfg(test)]
mod page_tests;
mod doors;
mod presentation;
mod sections;
mod source;

pub(super) fn body(
    place: &Route,
    route: &SymbolRoute,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    _hover: &mut HoverIntent,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let symbol = match crate::runtime::store::route_declaration(place) {
        Ok(symbol) => symbol,
        Err(unread) => return ctx.unread(&unread),
    };
    let resource = store.symbol(&symbol);
    let name = symbol.identity().name().to_owned();
    let page = match shown(&resource) {
        Shown::Ready(page) => page.clone(),
        other => return not_ready(&other, &PageKey::Symbol(symbol), &name, ctx, cx),
    };
    let package = route.package.as_str().to_owned();
    // The world's anatomy, when it knows this declaration (an enhancement,
    // never a gate: the index's page always draws).
    let anatomy = crate::model::pages::PackageRef::parse(&package)
        .ok()
        .and_then(|owner| crate::runtime::fixture_world::anatomy(&page.identity, &owner, cx));
    let packet = anatomy.as_ref().map(|anatomy| presentation::packet(anatomy, cx));
    let companions = companions::gather(&companions::of(&page), ctx.links, ctx.active, cx);
    let lede = teaser_source(&page.docs).map(|sentence| plain_markup(&sentence));
    let source = source::source(&page, &package, lede, anatomy.as_deref(), packet.as_deref(), &companions);
    let plan = facet::anatomy::plan::compile(&source);
    let geo = geometry(ctx);

    // Its own words, last: the docs after the lede (these record what they
    // say through `ctx`, so they are built first).
    let words = plan.sections.iter().any(|section| section.id == SectionId::Words).then(|| docs(&page, &package, ctx)).flatten();
    let title = title(&page, &plan, &geo, ctx, cx);
    let gem = gem(&page, &geo, ctx);
    let anchors = Anchors::new();
    let doors = doors::ShellDoors {
        package: package.clone(),
        symbol: symbol.clone(),
        links: ctx.links.clone(),
        targets: ctx.targets,
        active: ctx.active,
        disclosure: ctx.symbol_disclosure.clone(),
        reader: cx.weak_entity(),
        said: std::cell::RefCell::new(Vec::new()),
    };
    let mut bodies = Vec::new();
    for section in &plan.sections {
        let body = match section.id {
            SectionId::Does => sections::does(&page, &plan, anatomy.as_deref(), &geo, ctx, &doors, cx),
            SectionId::Fails => sections::fails(&page, anatomy.as_deref(), packet.as_deref(), ctx, &doors),
            SectionId::Uses => sections::uses(&page, anatomy.as_deref(), ctx, &doors),
            _ => None,
        };
        if let Some(body) = body {
            bodies.push((section.id, body));
        }
    }
    if let Some(words) = words {
        bodies.push((SectionId::Words, words));
    }
    let element = facet::anatomy::page::page(&plan, gem, title, bodies, geo, &anchors, &ctx.measure, ctx.palette, &doors);
    for said in doors.take_said() {
        ctx.say(said);
    }
    if ctx.active {
        facet::anatomy::page::publish(symbol.as_str().to_owned(), &anchors, cx);
    }
    let mut leaves = vec![Leaf::new(div().id("symbol-page").child(element))];
    if let Some(section) = upgrade(route, ctx, cx) {
        leaves.push(Leaf::new(section));
    }
    leaves
}

/// The page's geometry in the folio, with the reader's gutters beside it:
/// the margins carry the edges only when that room is there.
fn geometry(ctx: &Ctx<'_>) -> Geometry {
    let folio = ctx.measure.width();
    let reader = ctx.reader_scroll.bounds().size.width;
    let margin = if reader > folio { (reader - folio) / 2.0 } else { px(0.0) };
    Geometry::new(folio, margin, ctx.measure.scale())
}

/// The gem, shared by the declaration's address so the row, the page and
/// the graph node are one mark.
fn gem(page: &SymbolPage, geo: &Geometry, ctx: &Ctx<'_>) -> AnyElement {
    let kind = kind_of(page.identity.kind);
    let key = crate::shell::kit::shared_key(&page.identity.coordinate);
    let mark = facet::paint::gem(kind).size(geo.gem);
    div()
        .id(shared_id(&page.identity.coordinate))
        .debug_selector(move || key)
        .child(if ctx.active {
            facet::motion::shared::shared(shared_id(&page.identity.coordinate), mark)
                .timing(std::time::Duration::from_millis(460), facet::tokens::motion::GLIDE)
                .into_any_element()
        } else {
            mark.into_any_element()
        })
        .into_any_element()
}

/// The name, never ellipsized: it wraps at identifier boundaries and steps
/// down only when one segment cannot fit. A method's owner reads dim before
/// it (`FlagSet.`**`Parse`**).
fn title(page: &SymbolPage, plan: &facet::anatomy::plan::PagePlan, geo: &Geometry, ctx: &mut Ctx<'_>, cx: &gpui::App) -> AnyElement {
    let measure = ctx.measure;
    let name = ctx.say(plan.hero.name.clone());
    let owner = plan.hero.owner.as_ref().map(|owner| format!("{owner}."));
    let owner_w = owner.as_ref().map_or(px(0.0), |owner| facet::anatomy::page::words_w(owner.chars().count(), scale::DISPLAY, measure.scale()));
    let room = (geo.col_w - owner_w).max(px(120.0));
    let (lines, role) = crate::shell::text_fit::fit_name(&name, scale::DISPLAY, &measure, room, cx);
    ctx.hero.extend(lines.iter().map(|line| SharedString::from(line.clone())));
    let key = crate::shell::kit::shared_key(&page.identity.coordinate);
    let mut row = div().flex().flex_wrap().items_baseline();
    if let Some(owner) = owner {
        ctx.say(owner.clone());
        row = row.child(facet::anatomy::page::said("page-owner", owner, role, ctx.palette.ink3, &measure));
    }
    div()
        .id("page-title")
        .debug_selector(move || format!("page-title:{key}"))
        .child(row.child(crate::shell::text_fit::name_lines(&lines, role, ctx.palette.ink0)))
        .into_any_element()
}

/// Whether the docs say more than the lede.
pub(super) fn has_words(page: &SymbolPage) -> bool {
    let all = DocFragment::plain_text(&page.docs);
    let lede = teaser_source(&page.docs).map(|sentence| plain_markup(&sentence)).unwrap_or_default();
    all.trim().len() > lede.trim().len() + 1 || page.sections.sections.iter().any(|section| !presentation::is_failure(section.kind))
}

/// Away from the pin: the upgrade lens's section above the page (what
/// moving from the release you pin to the one viewed changes for this
/// declaration), when there is release data for its package.
fn upgrade(route: &SymbolRoute, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Option<AnyElement> {
    let at = route.at.as_ref()?;
    let pinned = crate::model::pages::PackageRef::parse(route.package.as_str()).ok()?;
    let diffs = crate::runtime::fixture_releases::release_data(&pinned, cx)?;
    let path = crate_path(&diffs.name, route.id.as_str());
    let to = crate::runtime::fixture_releases::spelled(diffs, at.as_str()).unwrap_or_else(|| at.as_str().to_owned().into());
    let lens = facet::data::release::lens(diffs, &path, &to, &facet::semantics::types::Nowhere);
    let pin = pinned.version().map_or_else(|| diffs.pinned.to_string(), ToOwned::to_owned);
    Some(
        facet::data::release::view::section("upgrade", lens, path, pin, &ctx.measure, &facet::anatomy::Links::plain(), ctx.reveal.xray)
            .into_any_element(),
    )
}

/// `toml::de::from_str` for a declaration of `krate` at `coordinate`: the
/// crate, its module (a file's stem, unless `lib`, `mod` or `main`), then
/// the declaration's own trail.
fn crate_path(krate: &str, coordinate: &str) -> String {
    let identity = backend_present::Identity::parse(coordinate);
    let mut parts = vec![krate.to_owned()];
    if let Some(path) = identity.path() {
        let stem = path.stem();
        if !stem.is_empty() && !matches!(stem, "lib" | "mod" | "main") {
            parts.push(stem.to_owned());
        }
    }
    parts.extend(identity.trail().segments().iter().map(|segment| segment.as_str().to_owned()));
    parts.join("::")
}

/// Docs as one styled text per paragraph; links are clickable ranges.
/// Source comments can begin with a punctuation divider. A teaser must be
/// an authored sentence with words; the divider remains source in Code.
fn separator_only(line: &str) -> bool {
    !line.chars().any(char::is_alphanumeric)
}

fn teaser_source(fragments: &[DocFragment]) -> Option<String> {
    let mut sentence = String::new();
    let mut breaks = 0;
    for fragment in fragments {
        if matches!(fragment, DocFragment::Break) { breaks += 1; continue; }
        // A single source line break is soft. Two mark a new paragraph; a
        // teaser never borrows words from another paragraph to finish one.
        if breaks >= 2 && !sentence.is_empty() { return None; }
        if breaks > 0 && !sentence.is_empty() && !sentence.chars().next_back().is_some_and(char::is_whitespace) { sentence.push(' '); }
        breaks = 0;
        match fragment {
            DocFragment::Text(text) => {
                for (index, line) in text.split('\n').enumerate() {
                    if separator_only(line.trim()) { continue; }
                    if index > 0 && !sentence.is_empty() && !sentence.chars().next_back().is_some_and(char::is_whitespace) { sentence.push(' '); }
                    sentence.push_str(line);
                }
            }
            DocFragment::Code(code) => { sentence.push('`'); sentence.push_str(code); sentence.push('`'); }
            DocFragment::Link { label, .. } => sentence.push_str(label),
            DocFragment::Break => unreachable!(),
        }
        if let Some(end) = sentence_end(&sentence) { return Some(sentence[..end].trim().to_owned()); }
        if sentence.len() > 480 { return None; }
    }
    None
}

fn sentence_end(markup: &str) -> Option<usize> {
    let mut code = false;
    let mut bracket = 0_u8;
    for (at, ch) in markup.char_indices() {
        match ch {
            '`' => code = !code,
            '[' if !code => bracket = bracket.saturating_add(1),
            ']' if !code => bracket = bracket.saturating_sub(1),
            '.' | '!' | '?' if !code && bracket == 0
                && markup[at + ch.len_utf8()..].chars().next().is_none_or(char::is_whitespace)
                && markup[..at].chars().filter(|ch| ch.is_alphabetic()).count() >= 3 => return Some(at + ch.len_utf8()),
            _ => {}
        }
    }
    None
}

fn plain_markup(markup: &str) -> String {
    facet::overlay::text::parse(markup).iter().map(facet::overlay::text::Piece::text).collect()
}

#[derive(Clone)]
enum DocJump { Exact(SymbolRef), Lookup(String) }

fn docs(page: &SymbolPage, package: &str, ctx: &mut Ctx<'_>) -> Option<AnyElement> {
    let fragments: Vec<DocFragment> = if page.sections.sections.is_empty() { page.docs.to_vec() } else {
        let mut fragments = page.sections.lead.to_vec();
        for section in page.sections.sections.iter().filter(|section| !presentation::is_failure(section.kind)) {
            fragments.push(DocFragment::Break);
            fragments.push(DocFragment::Break);
            fragments.push(DocFragment::Text(section.title.clone()));
            fragments.push(DocFragment::Break);
            fragments.push(DocFragment::Break);
            fragments.extend(section.body.iter().cloned());
            for entry in section.entries.iter() {
                fragments.push(DocFragment::Break);
                fragments.push(DocFragment::Break);
                fragments.push(DocFragment::Code(entry.subject.clone()));
                fragments.push(DocFragment::Text(" — ".into()));
                fragments.extend(entry.body.iter().cloned());
            }
        }
        fragments
    };
    if fragments.is_empty() { return None; }
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut paragraphs: Vec<(String, Vec<(Range<usize>, HighlightStyle)>, Vec<(Range<usize>, DocJump)>)> = vec![Default::default()];
    let mut breaks = 0;
    for fragment in &fragments {
        if matches!(fragment, DocFragment::Break) { breaks += 1; continue; }
        if breaks >= 2 { paragraphs.push(Default::default()); }
        else if breaks == 1 {
            let (text, _, _) = paragraphs.last_mut()?;
            if !text.is_empty() && !text.chars().next_back().is_some_and(char::is_whitespace) { text.push(' '); }
        }
        breaks = 0;
        match fragment {
            DocFragment::Text(prose) => {
                for (index, part) in prose.split("\n\n").enumerate() {
                    if index > 0 { paragraphs.push(Default::default()); }
                    let cleaned = part.lines().filter(|line| !separator_only(line.trim())).collect::<Vec<_>>().join(" ");
                    let (text, highlights, links) = paragraphs.last_mut()?;
                    for piece in facet::overlay::text::parse(&cleaned) {
                        let start = text.len();
                        text.push_str(piece.text());
                        let range = start..text.len();
                        match piece {
                            facet::overlay::text::Piece::Plain(_) => {}
                            facet::overlay::text::Piece::Code(_) => highlights.push((range, HighlightStyle {
                                color: Some(palette.ink0.into()), font_weight: Some(FontWeight(550.0)), ..HighlightStyle::default()
                            })),
                            facet::overlay::text::Piece::Emphasis(_) => highlights.push((range, HighlightStyle {
                                font_style: Some(FontStyle::Italic), ..HighlightStyle::default()
                            })),
                            facet::overlay::text::Piece::Strong(_) => highlights.push((range, HighlightStyle {
                                color: Some(palette.ink0.into()), font_weight: Some(FontWeight(700.0)), ..HighlightStyle::default()
                            })),
                            facet::overlay::text::Piece::Reference { target, .. } => {
                                if !["http:", "https:", "mailto:", "#"].iter().any(|prefix| target.starts_with(prefix)) {
                                    highlights.push((range.clone(), HighlightStyle { color: Some(palette.ink1.into()), ..HighlightStyle::default() }));
                                    links.push((range, DocJump::Lookup(target)));
                                }
                            }
                            facet::overlay::text::Piece::Shortcut { code, .. } => {
                                if code { highlights.push((range, HighlightStyle {
                                    color: Some(palette.ink0.into()), font_weight: Some(FontWeight(550.0)), ..HighlightStyle::default()
                                })); }
                            }
                        }
                    }
                }
            }
            DocFragment::Code(code) => {
                let (text, highlights, _) = paragraphs.last_mut()?;
                let start = text.len();
                text.push_str(code);
                highlights.push((
                    start..text.len(),
                    HighlightStyle {
                        color: Some(palette.ink0.into()),
                        font_weight: Some(FontWeight(550.0)),
                        ..HighlightStyle::default()
                    },
                ));
            }
            DocFragment::Link { label, coordinate, .. } => {
                let (text, highlights, links) = paragraphs.last_mut()?;
                let start = text.len();
                text.push_str(label);
                highlights.push((
                    start..text.len(),
                    HighlightStyle {
                        color: Some(palette.ink1.into()),
                        ..HighlightStyle::default()
                    },
                ));
                if let Some(target) = coordinate {
                    links.push((start..text.len(), DocJump::Exact(target.clone())));
                }
            }
            DocFragment::Break => unreachable!(),
        }
    }
    let mut column = div().flex().flex_col().gap(measure.space(Space::Roomy)).max_w(px(680.0 * measure.scale()));
    // The hero already says the first sentence: the body starts after it.
    let lede = teaser_source(&fragments).map(|text| plain_markup(&text));
    let mut removed_lede = false;
    for (index, (paragraph, highlights, links)) in paragraphs.into_iter().enumerate() {
        let mut shift = paragraph.len() - paragraph.trim_start().len();
        let mut trimmed = paragraph.trim().to_owned();
        if !removed_lede
            && let Some(lede) = &lede
            && let Some(rest) = trimmed.strip_prefix(lede.as_str())
        {
            removed_lede = true;
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
        let highlights = highlights.iter().filter_map(|(range, style)| { let range = clamp(range); (range.start < range.end).then_some((range, *style)) }).collect::<Vec<_>>();
        let (ranges, targets): (Vec<_>, Vec<_>) = links.into_iter().filter_map(|(range, target)| {
            let range = clamp(&range);
            (range.start < range.end).then_some((range, target))
        }).unzip();
        let dispatch = ctx.links.clone();
        let package = package.to_owned();
        let styled = StyledText::new(said).with_highlights(highlights);
        let interactive = InteractiveText::new(SharedString::from(format!("doc-{index}")), styled).on_click(ranges, move |which, _, cx| {
            match targets.get(which) {
                Some(DocJump::Exact(symbol)) => {
                    if let Some(route) = symbol_route(&package, symbol) { dispatch.dispatch(Intent::Navigate(route), cx); }
                }
                Some(DocJump::Lookup(path)) => {
                    if let Ok(query) = crate::model::pages::SearchQuery::new(path, 50) {
                        dispatch.dispatch(Intent::Navigate(Route::Orbit(crate::navigation::OrbitRoute::Browse(crate::navigation::BrowseRoute::Find(query)))), cx);
                    }
                }
                None => {}
            }
        });
        column = column.child(text(ty::PROSE, &measure, palette.ink1).child(interactive));
    }
    Some(column.into_any_element())
}


#[cfg(test)]
mod prose_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn teaser_joins_the_authors_two_line_error_sentence() {
        let docs = [
            DocFragment::Text(Arc::from("////////////////////.")),
            DocFragment::Break,
            DocFragment::Text(Arc::from("This type represents all possible errors that can occur when serializing or")),
            DocFragment::Break,
            DocFragment::Text(Arc::from("deserializing JSON data.")),
        ];
        assert_eq!(teaser_source(&docs).as_deref(), Some(
            "This type represents all possible errors that can occur when serializing or deserializing JSON data."
        ));
    }

    #[test]
    fn teaser_keeps_inline_code_and_reference_punctuation_inside_the_sentence() {
        let docs = [DocFragment::Text(Arc::from(
            "Read [the guide][crate::v1.2] before calling `value.get()` again. A second sentence follows."
        ))];
        let teaser = teaser_source(&docs).expect("authored sentence");
        assert_eq!(teaser, "Read [the guide][crate::v1.2] before calling `value.get()` again.");
        assert_eq!(plain_markup(&teaser), "Read the guide before calling value.get() again.");
    }
}
