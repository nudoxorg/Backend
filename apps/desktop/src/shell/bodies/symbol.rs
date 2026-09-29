//! The declaration page, simple form (`facet::anatomy::symbol`): docs.rs's
//! reading order, drawn. The header (the kind mark, the path, the name, the
//! author's lede), the call (one rail: each input a port, the rail's end how
//! it can end), the docs, if it fails, what it is (one of / holds / what you
//! write), what you can do with it, and in your workspace, with the context
//! rail beside it.
//!
//! The page is derived from the index's page for the declaration (`facts`
//! reads it into facet's plain `Facts`; `compile` is pure), and from the
//! places your packages name it (`uses` reads each line from the local file,
//! off the UI thread). The shell lends the page its doors, its folds and
//! filters (the reader's disclosure), opening a place in the editor
//! (`Intent::OpenSource`), and the shared elements a hop flies (the mark and
//! the title).

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf};
use crate::model::pages::{PageKey, SymbolPage};
use crate::navigation::{Route, SymbolRoute};
use crate::shell::kit::{HoverIntent, shared_id};
use crate::shell::reader::Reader;
use facet::anatomy::symbol::{Chrome, View};
use facet::tokens::scale;
use gpui::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div, px};

mod companions;
mod facts;
mod history;
mod host;
#[cfg(test)]
mod hop_tests;
#[cfg(test)]
mod page_tests;
mod place;
mod uses;

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
    let companions = companions::gather(&companions::of(&page), ctx.links, ctx.active, cx);
    let history = history_of(&package, &page, cx);
    let facts = facts::facts(&page, &package, &companions, &history);
    let view = facet::anatomy::symbol::compile(&facts);
    // What your packages do with it: the lines the page carries, read.
    let workspace = facet::anatomy::symbol::derive::uses::read_all(&uses::sites(&page), &facet::anatomy::symbol::derive::uses::Reader::of(&view));
    let view = facet::anatomy::symbol::with_uses(view, &workspace);

    let disclosure = ctx.symbol_disclosure.clone();
    let host = host::ShellHost {
        package: package.clone(),
        symbol: symbol.clone(),
        links: ctx.links.clone(),
        targets: ctx.targets,
        active: ctx.active,
        from: ctx.arrived_from.clone(),
        disclosure,
        reader: cx.weak_entity(),
        scroll: ctx.reader_scroll.clone(),
        said: std::cell::RefCell::new(Vec::new()),
    };
    let lay = facet::anatomy::symbol::layout::Layout::of(&ctx.measure, &ctx.modes);
    let title = title(&page, &facts.name, facts.owner.as_deref(), lay.main, ctx, cx);
    let gem = gem(&page, &view, ctx);
    let element = facet::anatomy::symbol::page(&view, &workspace, Chrome { gem, title }, &ctx.measure, ctx.palette, &host, &ctx.modes);
    for said in host.take_said() {
        ctx.say(said);
    }
    let mut leaves = vec![Leaf::new(div().id("symbol-page").child(element))];
    if let Some(section) = upgrade(route, ctx, cx) {
        leaves.push(Leaf::new(section));
    }
    leaves
}

/// The mark, shared by the declaration's address so the row, the page and
/// the graph node are one mark: the kind's mark, the family's hue.
fn gem(page: &SymbolPage, view: &View, ctx: &Ctx<'_>) -> AnyElement {
    let key = crate::shell::kit::shared_key(&page.identity.coordinate);
    let mark = facet::anatomy::symbol::gem(view, &ctx.measure, ctx.palette);
    div()
        .id(shared_id(&page.identity.coordinate))
        .debug_selector(move || key)
        .child(if ctx.active {
            facet::motion::shared::shared(shared_id(&page.identity.coordinate), mark)
                .timing(std::time::Duration::from_millis(460), facet::tokens::motion::GLIDE)
                .into_any_element()
        } else {
            mark
        })
        .into_any_element()
}

/// Its history across the releases the release data has read.
fn history_of(package: &str, page: &SymbolPage, cx: &mut gpui::App) -> facet::anatomy::history::History {
    let Ok(pinned) = crate::model::pages::PackageRef::parse(package) else { return facet::anatomy::history::History::default() };
    let Some(krate) = crate::runtime::fixture_releases::release_data(&pinned, cx) else { return facet::anatomy::history::History::default() };
    let path = crate_path(&krate.name, page.identity.coordinate.as_str());
    history::of(&pinned, page, &path, cx)
}

/// The name, never ellipsized: it wraps at identifier boundaries and steps
/// down only when one segment cannot fit. A method's owner reads dim before
/// it (`FlagSet.`**`Parse`**).
///
/// The title is shared under its declaration's title key, the key every
/// row that opens it carries: on an Open the reader drives it from the
/// clicked row's name along the plate's edge. It is re-set in the title face
/// at the size it is seen at each frame (from the row's size on frame one),
/// never a scaled bitmap: its box hugs the name, and the text inside is laid
/// out at the painted size and counter-scaled.
fn title(page: &SymbolPage, name: &str, owner: Option<&str>, room: gpui::Pixels, ctx: &mut Ctx<'_>, cx: &gpui::App) -> AnyElement {
    let measure = ctx.measure;
    let name = ctx.say(name.to_owned());
    let owner = owner.map(|owner| format!("{owner}."));
    let owner_w = owner.as_ref().map_or(px(0.0), |owner| facet::anatomy::page::words_w(owner.chars().count(), scale::DISPLAY, measure.scale()));
    let room = (room - owner_w).max(px(120.0));
    let (lines, role) = crate::shell::text_fit::fit_name(&name, scale::DISPLAY, &measure, room, cx);
    ctx.hero.extend(lines.iter().map(|line| SharedString::from(line.clone())));
    if let Some(owner) = &owner {
        ctx.say(owner.clone());
    }
    let owner_w = owner.as_ref().map_or(px(0.0), |owner| crate::shell::text_fit::text_width(owner, &role, cx));
    let own_w = owner_w + lines.iter().map(|line| crate::shell::text_fit::text_width(line, &role, cx)).fold(px(0.0), gpui::Pixels::max) + px(2.0);
    let own_h: f32 = role.line * lines.len().max(1) as f32;
    let (ink0, ink3) = (ctx.palette.ink0.hsla(), ctx.palette.ink3.hsla());
    let set = move |role: facet::tokens::TypeRole| {
        let mut row = div().flex().items_start();
        if let Some(owner) = owner.clone() {
            row = row.child(facet::probe::text(
                gpui::ElementId::Name("page-owner".into()),
                SharedString::from(owner.clone()),
                role,
                1.0,
                facet::probe::TextOverflow::Clip,
                facet::fonts::Typeset::typeset_at(div().whitespace_nowrap().text_color(ink3), role, 1.0).child(owner),
            ));
        }
        row.child(crate::shell::text_fit::name_lines(&lines, role, ink0))
    };
    let key = crate::shell::kit::shared_key(&page.identity.coordinate);
    let content = if ctx.active {
        facet::motion::shared::shared_with(facet::anatomy::page::title_key(page.identity.coordinate.as_str()), move |morph| {
            let own = own_h;
            let painted = morph.painted(own).max(1.0);
            let k = painted / own;
            let seen = facet::tokens::TypeRole { size: role.size * k, line: role.line * k, ..role };
            div().w(own_w).h(px(own_h)).child(gpui::layer(set(seen)).scale(own / painted).origin(0.0, 0.0))
        })
        .timing(std::time::Duration::from_millis(460), facet::tokens::motion::GLIDE)
        .into_any_element()
    } else {
        div().w(own_w).h(px(own_h)).child(set(role)).into_any_element()
    };
    div().id("page-title").debug_selector(move || format!("page-title:{key}")).child(content).into_any_element()
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

