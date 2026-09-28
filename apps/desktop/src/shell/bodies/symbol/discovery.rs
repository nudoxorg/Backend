//! A small invitation into a large relation neighborhood. Opening a verb
//! reveals scope; opening a package reveals names. The graph remains a door.

use super::*;
use crate::shell::reader::SymbolFold;
use facet::semantics::relations::rows::{Band, Name, RelationRow, meaning, verb as page_verb};
use facet::semantics::{Word, Target as WorldTarget};
use facet::graph::World;
use facet::Set;
use gpui::{ElementId, PathBuilder, canvas, point};
use std::sync::Arc;

pub(super) fn world_rows(page: &SymbolPage, world: &World, rows: &[RelationRow], words: &[Word], exclude: bool, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let limit = if measure.effective() < 500.0 { 1 } else { 3 };
    let mut root = div().flex().flex_col().gap(measure.space(Space::Roomy));
    for row in rows.iter().filter(|row| row.word != Word::Is && (words.contains(&row.word) != exclude)) {
        let word = page_verb(row.word);
        let fold = SymbolFold::Relations(word);
        let open = ctx.symbol_disclosure.is_open(&fold);
        let mut names = div().flex().flex_wrap().min_w_0().gap(measure.space(Space::Base));
        for name in row.names.iter().take(limit) { names = names.child(world_name(world, name, &format!("{word}-invitation"), true, ctx)); }
        let more = row.names.len().saturating_sub(limit);
        if more > 0 || open {
            let label = if open { "Fold packages".to_owned() } else { format!("and {more} more") };
            names = names.child(super::fold_control(page, fold.clone(), label, ctx, cx));
        }
        let said = ctx.say(word);
        let definition = meaning(row.word);
        let key: SharedString = format!("relation-verb-{word}").into();
        let request_key = key.clone();
        let link = facet::overlay::text::Link::new(key.clone(), move |rect| {
            facet::overlay::float::FloatRequest::new(request_key.clone(), rect, facet::overlay::float::FloatKind::Tip, move |m, _, cx| {
                use facet::ActiveFacet;
                let p = cx.facet().palette();
                div().set(ty::SMALL, m).text_color(p.ink1.hsla()).p(m.space(Space::Base)).child(definition).into_any_element()
            })
        });
        let verb = facet::overlay::text::words(key, StyledText::new(said.clone()), vec![(0..said.len(), link)], palette);
        let heading = div().set(ty::SMALL, &measure).text_color(palette.ink3.hsla()).child(verb);
        let main = if measure.effective() < 520.0 {
            div().flex().flex_col().gap(measure.space(Space::Tight)).child(heading).child(names)
        } else {
            div().flex().items_start().gap(measure.space(Space::Roomy))
                .child(heading.w(px(104.0 * measure.scale())).flex_none().text_right())
                .child(names.flex_1())
        };
        let motion = ctx.symbol_disclosure.unroll(fold.clone());
        let mut detail = div().flex().flex_col().gap(measure.space(Space::Roomy));
        if open { detail = detail.child(text(ty::SMALL, &measure, palette.ink3).child(ctx.say(row.count()))); }
        let previous_active = ctx.active;
        ctx.active &= open;
        for scope in row.scopes.iter().filter(|_| open || !motion.is_empty()) {
            let count = scope.packages.iter().map(|p| p.names.len()).sum::<usize>();
            let mut band = div().flex().flex_col().gap(measure.space(Space::Base));
            let label = format!("{} · {count} in {} packages", scope.band.text(), scope.packages.len());
            band = band.child(text(ty::SMALL, &measure, palette.ink1).child(label));
            for package in scope.packages.iter().take(12) {
                let package_fold = SymbolFold::RelationPackage(word, package.package.unwrap_or(u32::MAX));
                let expanded = ctx.symbol_disclosure.is_open(&package_fold);
                let package_key = package.package.unwrap_or(u32::MAX);
                let mut names = div().flex().flex_wrap().min_w_0().gap(measure.space(Space::Base));
                for name in package.names.iter().take(limit) { names = names.child(world_name(world, name, &format!("{word}-package-{package_key}-preview"), open, ctx)); }
                let remaining = package.names.len().saturating_sub(limit);
                if remaining > 0 {
                    names = names.child(super::fold_control(page, package_fold.clone(), if expanded { "Fold package".to_owned() } else { format!("+ {remaining}") }, ctx, cx));
                }
                let motion = ctx.symbol_disclosure.unroll(package_fold);
                let mut extra = div().flex().flex_wrap().min_w_0().gap(measure.space(Space::Base));
                if expanded || !motion.is_empty() {
                    for name in package.names.iter().skip(limit).take(100-limit) { extra = extra.child(world_name(world, name, &format!("{word}-package-{package_key}-extra"), open && expanded, ctx)); }
                    if package.names.len() > 100 { extra = extra.child(graph_control_at(format!("{} more in graph", package.names.len()-100), &format!("{word}-package-{package_key}"), ctx)); }
                }
                let package_label = format!("{}  {}", package.name, package.names.len());
                let line = div().flex().flex_col().gap(measure.space(Space::Tight))
                    .child(text(ty::MONO_SMALL, &measure, palette.ink3).child(package_label)).child(names)
                    .child(facet::anatomy::unroll::unroll(format!("relation-package-{word}-{package_key}"), open && expanded, motion, extra));
                band = band.child(line);
            }
            if scope.packages.len() > 12 { band = band.child(graph_control_at(format!("Explore {} more packages in graph", scope.packages.len() - 12), &format!("{word}-{:?}", scope.band), ctx)); }
            detail = detail.child(band);
        }
        ctx.active = previous_active;
        let detail = facet::anatomy::unroll::unroll(format!("relations-{word}"), open, motion, detail);
        root = root.child(div().flex().flex_col().gap(measure.space(Space::Base)).child(main).child(detail));
    }
    root.into_any_element()
}

fn world_name(world: &World, name: &Name, occurrence: &str, interactive: bool, ctx: &mut Ctx<'_>) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let said = ctx.say(name.label.to_string());
    let Some(node) = name.entry.node else { return text(ty::MONO_ROW, &measure, palette.ink2).child(said).into_any_element(); };
    let id: SharedString = format!("world-rel-{occurrence}-{node}").into();
    let act: Act = Rc::new(move |window, cx| window.dispatch_action(Box::new(facet::anatomy::Open { target: WorldTarget::Node(node) }), cx));
    if interactive && ctx.active { ctx.targets.push(Target { id: id.clone(), label: said.clone(), act: Rc::clone(&act), peek: None, source: None }); }
    ctx.targets.track(id.clone(), div().id(id).flex().items_center().gap(px(5.0 * measure.scale()))
        .min_h(px(28.0 * measure.scale()))
        .child(crate::shell::kit::kind_mark(crate::shell::kit::world_kind(world.node(node).kind), KindSize::Sm, &measure, palette))
        .child(text(ty::MONO_ROW, &measure, palette.ink1).child(said))
        .on_click(move |_, window, cx| act(window, cx))).into_any_element()
}

pub(super) fn graph_control(label: String, ctx: &mut Ctx<'_>) -> AnyElement {
    graph_control_at(label, "default", ctx)
}

fn graph_control_at(label: String, occurrence: &str, ctx: &mut Ctx<'_>) -> AnyElement {
    let links = ctx.links.clone();
    let id: SharedString = format!("symbol-graph-{occurrence}-{label}").into();
    let label = ctx.say(label);
    let act: Act = Rc::new(move |_, cx| links.dispatch(Intent::SetView(View::Graph), cx));
    if ctx.active { ctx.targets.push(Target { id: id.clone(), label: label.clone(), act: act.clone(), peek: None, source: None }); }
    ctx.targets.track(id.clone(), div().id(id).flex().items_center().gap(ctx.measure.space(Space::Base))
        .min_h(px(28.0 * ctx.measure.scale()))
        .child(icons::ui(Icon::Rose, IconSize::S16, ctx.palette.peri.base).size(ctx.measure.icon(14.0)))
        .child(text(ty::SMALL, &ctx.measure, ctx.palette.ink1).child(label))
        .on_click(move |_, window, cx| act(window, cx))).into_any_element()
}

/// A miniature of the actual groups, so the door previews what it opens.
pub(super) fn door(rows: &[RelationRow], ctx: &mut Ctx<'_>) -> AnyElement {
    let palette = ctx.palette;
    let groups = rows.iter().map(|r| (matches!(r.word, Word::MadeBy | Word::ImplementedBy | Word::CalledFrom), r.names.len(), r.yours > 0)).collect::<Vec<_>>();
    let drawing = canvas(|_, _, _| {}, move |bounds, (), window, _| {
        let c = bounds.center();
        let mut path = PathBuilder::stroke(px(1.0));
        let r = px(4.0);
        path.move_to(point(c.x, c.y-r)); path.line_to(point(c.x+r, c.y)); path.line_to(point(c.x, c.y+r)); path.line_to(point(c.x-r, c.y)); path.close();
        if let Ok(path) = path.build() { window.paint_path(path, palette.peri.base.hsla()); }
        let total = groups.len().max(1);
        for (n, &(incoming, count, yours)) in groups.iter().enumerate() {
            let direction = if incoming { -1.0 } else { 1.0 };
            let length = (count as f32 + 1.0).ln().mul_add(6.0, 10.0).min(34.0);
            let y = bounds.top() + bounds.size.height * ((n as f32 + 1.0)/(total as f32 + 1.0));
            let mut path = PathBuilder::stroke(px(1.0));
            path.move_to(point(c.x + px(direction*5.0), c.y)); path.line_to(point(c.x + px(direction*length), y));
            if let Ok(path) = path.build() { window.paint_path(path, if yours { palette.mint.base.hsla() } else { palette.ink3.hsla() }); }
        }
    }).w(px(80.0 * ctx.measure.scale())).h(px(28.0 * ctx.measure.scale()));
    div().flex().items_center().gap(ctx.measure.space(Space::Base)).child(drawing)
        .child(graph_control("Explore graph".to_owned(), ctx)).into_any_element()
}
