//! The package page, plain: hero, one facts line, "start with" (the
//! package's top-level declarations as mark + name rows), dependencies, and
//! the README's first blocks.

use super::state::{Shown, not_ready, shown};
use super::{Ctx, Leaf};
use crate::model::local_package::ReadmeBlock;
use crate::model::pages::{OutlineNode, PackageDossier, PackageRef, PageKey};
use crate::navigation::{Intent, Route};
use crate::shell::focus::{Act, Target};
use crate::shell::kit::{HoverIntent, gap_words, kind_of, quiet, symbol_route, text};
use crate::shell::reader::Reader;
use facet::icons::{self, Kind, KindSize};
use facet::tokens::ty;
use facet::{Measure, Palette, Space};
use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};
use std::rc::Rc;

pub(super) fn body(
    place: &Route,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    _hover: &mut HoverIntent,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    let Some(package) = crate::runtime::store::route_package(place) else {
        return vec![Leaf::new(quiet("This page's address is not a package.", &ctx.measure, ctx.palette))];
    };
    let resource = store.package(&package);
    let dossier = match shown(&resource) {
        Shown::Ready(dossier) => dossier.clone(),
        other => return not_ready(&other, &PageKey::Package(package.clone()), package.display_name(), ctx, cx),
    };
    let mut leaves = vec![hero(&dossier, ctx)];
    leaves.push(start_with(&dossier, ctx, cx));
    if let Some(leaf) = dependencies(&dossier, ctx) {
        leaves.push(leaf);
    }
    if let Some(leaf) = readme(&dossier, ctx) {
        leaves.push(leaf);
    }
    leaves
}

fn hero(dossier: &PackageDossier, ctx: &mut Ctx<'_>) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let record = dossier.record.known();
    let name = ctx.say(
        record
            .map_or_else(|| dossier.package.display_name().to_owned(), |record| record.name.to_string()),
    );
    let mut words = div().flex().flex_col().gap(measure.space(Space::Tight)).child(text(ty::HERO, &measure, palette.ink0).child(name));
    if let Some(description) = record.and_then(|record| record.description.known()) {
        let lede = ctx.say(description.to_string());
        words = words.child(text(ty::LEDE, &measure, palette.ink2).child(lede));
    }
    let mut facts = Vec::new();
    if let Some(version) = record.and_then(|record| record.version.known()) {
        facts.push(version.to_string());
    }
    if let Some(ecosystem) = record.and_then(|record| record.ecosystem.known()) {
        facts.push(ecosystem.to_string());
    }
    if let Some(license) = record.and_then(|record| record.license.known()) {
        facts.push(license.to_string());
    }
    if let Some(tree) = dossier.outline.known() {
        facts.push(format!("{} declarations", tree.count()));
    }
    let line = ctx.say(facts.join("  ·  "));
    Leaf::new(
        div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Roomy))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Wide))
                    .child(facet::paint::gem(Kind::Package).size(f32::from(measure.fluid(48.0, 64.0))))
                    .child(words),
            )
            .child(text(ty::SMALL, &measure, palette.ink3).child(line)),
    )
}

fn start_with(dossier: &PackageDossier, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let heading = ctx.say("Start with");
    let mut column = div().flex().flex_col().child(head(heading, &measure, palette));
    match dossier.outline.known() {
        Some(tree) => {
            for node in tree.roots.iter().take(60) {
                column = column.child(node_row(node, &dossier.package, ctx, cx));
            }
            if !tree.complete {
                let more = ctx.say("The outline is still being read.");
                column = column.child(quiet(more, &measure, palette));
            }
        }
        None => {
            if let Some(gap) = dossier.outline.gap() {
                let words = ctx.say(gap_words(gap));
                column = column.child(quiet(words, &measure, palette));
            }
        }
    }
    Leaf::new(column)
}

fn node_row(node: &OutlineNode, package: &PackageRef, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let symbol = node.decl.coordinate.clone();
    let id: SharedString = format!("pkg-{}", symbol.as_str()).into();
    let name = ctx.say(node.decl.name.to_string());
    let route = symbol_route(package.as_str(), &symbol);
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
        peek: Some(PageKey::Symbol(symbol.clone())),
        source: None,
    });
    let count = node.count().saturating_sub(1);
    let warm = PageKey::Symbol(symbol);
    let row = div()
        .id(id.clone())
        .flex()
        .items_center()
        .gap(measure.space(Space::Roomy))
        .h(measure.row() + measure.space(Space::Base))
        .px(measure.space(Space::Base))
        .hover(|style| style.bg(palette.tint))
        .child(crate::shell::kit::kind_mark(kind_of(node.decl.kind), KindSize::Sm, &measure, palette))
        .child(text(ty::MONO_ROW, &measure, palette.ink1).child(name))
        .children((count > 0).then(|| text(ty::SMALL, &measure, palette.ink4).child(count.to_string())))
        .on_click(move |_: &ClickEvent, window, cx| act(window, cx))
        .on_hover(cx.listener(move |reader, hovered: &bool, _, cx| reader.hover_link(warm.clone(), *hovered, cx)));
    ctx.targets.track(id, row).into_any_element()
}

fn dependencies(dossier: &PackageDossier, ctx: &mut Ctx<'_>) -> Option<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let list = dossier.dependencies.known()?;
    if list.is_empty() {
        return None;
    }
    let heading = ctx.say("Depends on");
    let mut column = div().flex().flex_col().child(head(heading, &measure, palette));
    for dependency in list.iter() {
        let name = ctx.say(dependency.name.to_string());
        column = column.child(
            div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Roomy))
                .h(measure.row() + measure.space(Space::Tight))
                .px(measure.space(Space::Base))
                .child(crate::shell::kit::kind_mark(Kind::Package, KindSize::Sm, &measure, palette))
                .child(text(ty::MONO_ROW, &measure, palette.ink1).child(name))
                .child(text(ty::MONO_SMALL, &measure, palette.ink3).child(dependency.requirement.to_string()))
                .children((dependency.scope != crate::model::pages::DependencyScope::Runtime).then(|| {
                    text(ty::SMALL, &measure, palette.ink4).child(dependency.scope.name())
                })),
        );
    }
    Some(Leaf::new(column))
}

fn readme(dossier: &PackageDossier, ctx: &mut Ctx<'_>) -> Option<Leaf> {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let blocks = dossier.readme.known()?;
    if blocks.is_empty() {
        return None;
    }
    let heading = ctx.say("Read me");
    let mut column = div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Base))
        .max_w(px(680.0 * measure.scale()))
        .child(head(heading, &measure, palette));
    for block in blocks.iter().take(24) {
        column = column.child(readme_block(block, ctx));
    }
    Some(Leaf::new(column))
}

fn readme_block(block: &ReadmeBlock, ctx: &mut Ctx<'_>) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let (role, words, ink) = match block {
        ReadmeBlock::Heading { text: words, .. } => (ty::HEAD, words.to_string(), palette.ink0),
        ReadmeBlock::Paragraph(words) => (ty::PROSE, words.to_string(), palette.ink1),
        ReadmeBlock::Code { text: words, .. } => (ty::CODE, words.to_string(), palette.ink1),
        ReadmeBlock::Bullet(words) => (ty::PROSE, format!("· {words}"), palette.ink1),
    };
    let said = ctx.say(words);
    text(role, &measure, ink).child(said).into_any_element()
}

fn head(title: SharedString, measure: &Measure, palette: &Palette) -> AnyElement {
    text(ty::TITLE, measure, palette.ink0)
        .pt(measure.space(Space::Wide))
        .pb(measure.space(Space::Base))
        .child(title)
        .into_any_element()
}
