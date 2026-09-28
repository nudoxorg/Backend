//! The declaration page: hero, one facts line, the lens bar, then either
//! the anatomy (when the world knows the declaration: its shape, `can`
//! line, Getting one / Calling it, docs, relation list and Does, with real
//! statements under Usage) or, plain, the declaration's code, its docs, the
//! relation list (the rose's narrow form) and the Made of / Does ledgers as
//! mark + name + one sentence rows.

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
    AnyElement, ClickEvent, Context, HighlightStyle, Hsla, InteractiveElement,
    FontStyle, FontWeight, InteractiveText, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, StyledText, div, px,
};
use std::ops::Range;
use std::rc::Rc;

mod presentation;
mod discovery;
use presentation::Section;
use crate::shell::reader::SymbolFold;

pub(super) fn body(
    place: &Route,
    route: &SymbolRoute,
    store: &super::Pages,
    ctx: &mut Ctx<'_>,
    hover: &mut HoverIntent,
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
    // The anatomy, when the world knows this declaration; otherwise the body
    // built from the index's page data (`runtime::fixture_world`).
    let anatomy = crate::model::pages::PackageRef::parse(&package)
        .ok()
        .and_then(|owner| crate::runtime::fixture_world::anatomy(&page.identity, &owner, cx));
    let packet = anatomy.as_ref().map(|anatomy| presentation::packet(anatomy, cx));
    let mut leaves = vec![hero(&page, ctx, cx), sentence(&page, anatomy.as_deref(), packet.as_deref(), ctx, cx), lens_bar(ctx, cx)];
    if ctx.lens == Lens::Reference
        && let Some(section) = upgrade(route, ctx, cx)
    {
        leaves.push(Leaf::new(section));
    }
    match ctx.lens {
        Lens::Reference => {
            if let Some(anatomy) = &anatomy {
                leaves.extend(anatomy_reference(&page, anatomy, packet.as_deref().expect("anatomy packet"), &package, ctx, hover, cx));
            } else {
                leaves.push(Leaf::new(indexed_shape(&page, &package, ctx, hover, cx)));
                leaves.extend(ledgers(&page, &package, ctx, hover, cx));
                if let Some(failures) = documented_failures(&page, ctx) { leaves.push(Leaf::new(failures)); }
                leaves.push(Leaf::new(div().flex().flex_col().child(section_heading(Section::Connections, ctx))
                    .child(relations(&page, &package, ctx, hover, cx, true))));
                if let Some(docs) = docs(&page, &package, ctx) {
                    leaves.push(Leaf::new(div().flex().flex_col().child(section_heading(Section::Documentation, ctx)).child(docs)));
                }
            }
        }
        Lens::Relations => leaves.push(Leaf::new(relations(&page, &package, ctx, hover, cx, false))),
        Lens::Usage => {
            // Real statements from the callers' bodies first, then every
            // indexed use.
            if let Some(anatomy) = anatomy.as_ref().filter(|anatomy| !anatomy.uses.is_empty()) {
                let links = crate::runtime::fixture_world::links(&anatomy.world);
                leaves.push(Leaf::new(
                    facet::anatomy::in_use("anatomy-in-use", anatomy.uses.clone(), &ctx.measure, &links)
                        .into_any_element(),
                ));
            }
            leaves.push(Leaf::new(usage(&page, ctx)));
        }
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
    let lede = teaser_source(&page.docs).map(|sentence| plain_markup(&sentence));
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
        .child(if ctx.active {
            facet::motion::shared::shared(shared_id(&page.identity.coordinate),
                facet::paint::gem(kind).size(gem_size))
                .timing(std::time::Duration::from_millis(460), facet::tokens::motion::GLIDE).into_any_element()
        } else { facet::paint::gem(kind).size(gem_size).into_any_element() });
    let top = if stacked {
        div().flex().flex_col().gap(measure.space(Space::Roomy)).child(gem).child(words)
    } else {
        div().flex().items_center().gap(gap).child(gem).child(words)
    };
    // Identity facts are individual glyph-bearing marks. Section clauses
    // carry counts; no field list competes with the subject and its purpose.
    let mut facts = vec![(Icon::Tag, page.identity.kind_name().to_owned()), (Icon::Folder, where_is(&page.identity))];
    if let (Some(path), Some(line)) = (&page.identity.path, page.identity.line) {
        facts.push((Icon::File, format!("{path}:{line}")));
    }
    // Uses speak only when there are some (said once, never "0 uses").
    let uses = page.references.known().map(|sites| sites.len()).filter(|uses| *uses > 0);
    let mut line = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(measure.space(Space::Base))
        .set_text(ty::SMALL, &measure, palette.ink3);
    // A separator travels with the fact after it, so a wrapped line never
    // ends on a dot.
    for (glyph, fact) in facts {
        let fact = ctx.say(fact);
        line = line.child(
            div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Base))
                .whitespace_nowrap()
                .child(icons::ui(glyph, IconSize::S12, palette.ink3).size(measure.icon(12.0)))
                .child(fact),
        );
    }
    if let Some(notice) = page.identity.facts.deprecated() {
        let note = notice.note.as_deref().unwrap_or("The author marks this declaration deprecated.");
        let since = notice.since.as_deref().map(|since| format!(" since {since}")).unwrap_or_default();
        line = line.child(div().flex().items_center().gap(measure.space(Space::Base))
            .child(icons::ui(Icon::Alert, IconSize::S12, palette.coral.base).size(measure.icon(12.0)))
            .child(text(ty::SMALL, &measure, palette.ink1).child(ctx.say(format!("Deprecated{since}: {note}")))));
    }
    if let Some(uses) = uses {
        let said = ctx.say(format!("{uses} uses"));
        line = line.child(
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .whitespace_nowrap()
                .child(dot(palette))
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

fn section_heading(section: Section, ctx: &mut Ctx<'_>) -> AnyElement {
    let title = ctx.say(section.title());
    ctx.targets.track(section.anchor(), section_head(title, &ctx.measure, ctx.palette)).into_any_element()
}

fn fold_control(page: &SymbolPage, fold: SymbolFold, label: String, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> AnyElement {
    let id: SharedString = format!("symbol-fold-{fold:?}").into();
    let symbol = page.identity.coordinate.clone();
    let open = ctx.symbol_disclosure.is_open(&fold);
    let weak = cx.weak_entity();
    let act: Act = Rc::new(move |_, cx| { let _ = weak.update(cx, |reader, cx| reader.toggle_symbol(symbol.clone(), fold.clone(), cx)); });
    let label = ctx.say(label);
    if ctx.active { ctx.targets.push(Target { id: id.clone(), label: label.clone(), act: act.clone(), peek: None, source: None }); }
    ctx.targets.track(id.clone(), div().id(id).flex().items_center().gap(ctx.measure.space(Space::Base))
        .min_h(px(30.0 * ctx.measure.scale()))
        .child(icons::ui(if open { Icon::Layers } else { Icon::More }, IconSize::S12, ctx.palette.peri.base).size(ctx.measure.icon(12.0)))
        .child(text(ty::SMALL, &ctx.measure, ctx.palette.ink1).child(label))
        .on_click(move |_, window, cx| act(window, cx))).into_any_element()
}

fn sentence(page: &SymbolPage, anatomy: Option<&crate::runtime::fixture_world::Anatomy>, packet: Option<&presentation::WorldPacket>, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Leaf {
    let members = page.members.known();
    let shape = if let Some(anatomy) = anatomy {
        use facet::semantics::page::Shape;
        match &anatomy.page.shape {
            Shape::Fork(fork) => Some(format!("{} {}", fork.branches.len(), if fork.branches.len() == 1 { "variant" } else { "variants" })),
            Shape::Holds(holds) => Some(format!("{} {}", holds.fields.len(), if holds.fields.len() == 1 { "field" } else { "fields" })),
            Shape::Pipe(pipe) => Some(format!("{} {}", pipe.inputs.len(), if pipe.inputs.len() == 1 { "input" } else { "inputs" })),
            Shape::Contract(contract) => Some(format!("{} required {}", contract.write.len(), if contract.write.len() == 1 { "operation" } else { "operations" })),
            _ => None,
        }
    } else { None };
    let lead = shape.map_or_else(
        || format!("{} is a {}.", page.identity.name, page.identity.kind_name()),
        |shape| format!("{} has {shape}.", page.identity.name),
    );
    let mut clauses = vec![(Section::Shape, lead)];
    if let Some(recipe) = anatomy.and_then(|a| a.recipe.as_ref()) {
        clauses.push((Section::Getting, if recipe.heading == "Calling it" { "the call path" } else { "ways to make one" }.to_owned()));
    }
    if anatomy.is_some_and(|a| !a.page.does.is_empty() || !a.page.caps.is_empty()) || members.is_some_and(|m| !m.does.is_empty()) {
        clauses.push((Section::Behavior, "operations".to_owned()));
    }
    let has_documented_failures = page.sections.sections.iter().any(|s| presentation::is_failure(s.kind))
        || members.is_some_and(|m| m.all().any(|m| m.sections.sections.iter().any(|s| presentation::is_failure(s.kind))));
    if has_documented_failures || packet.is_some_and(|p| p.failure.is_some() || p.callable_failure.as_ref().is_some_and(|f| f.all > 0)) {
        clauses.push((Section::Failure, "failure details".to_owned()));
    }
    if packet.is_some_and(|p| p.relations.iter().any(|r| matches!(r.word, facet::semantics::Word::ImplementedBy | facet::semantics::Word::TakenBy | facet::semantics::Word::HeldBy | facet::semantics::Word::CallsIt | facet::semantics::Word::UsedBy | facet::semantics::Word::CalledFrom | facet::semantics::Word::Calls))) || page.references.known().is_some_and(|s| !s.is_empty()) {
        clauses.push((Section::Connections, "connections".to_owned()));
    }
    if !page.docs.is_empty() { clauses.push((Section::Documentation, "author notes".to_owned())); }
    let count = clauses.len();
    let mut line = div().flex().flex_wrap().items_baseline()
        .gap_x(ctx.measure.space(Space::Tight)).gap_y(ctx.measure.space(Space::Tight));
    for (index, (section, label)) in clauses.into_iter().enumerate() {
        let phrase = if index == 0 { label }
            else if index == 1 { format!("Explore {label}{}", if count == 2 { "." } else { "," }) }
            else if index + 1 == count { format!("and {label}.") }
            else { format!("{label},") };
        let id: SharedString = format!("sentence-{}", section.anchor()).into();
        let weak = cx.weak_entity();
        let act: Act = Rc::new(move |_, cx| { let _ = weak.update(cx, |reader, cx| { reader.set_lens(Lens::Reference, cx); reader.jump_symbol_section(section.anchor(), cx); }); });
        let label = ctx.say(phrase);
        if ctx.active { ctx.targets.push(Target { id: id.clone(), label: label.clone(), act: act.clone(), peek: None, source: None }); }
        let underline = ctx.palette.line2.hsla();
        let clause = div().id(id.clone()).flex().items_baseline().whitespace_nowrap()
            .hover(move |style| style.border_b_1().border_color(underline))
            .child(text(ty::BODY, &ctx.measure, ctx.palette.ink1).child(label))
            .on_click(move |_, window, cx| act(window, cx));
        line = line.child(ctx.targets.track(id, clause));
    }
    Leaf::new(line)
}

fn indexed_shape(page: &SymbolPage, package: &str, ctx: &mut Ctx<'_>, hover: &mut HoverIntent, cx: &mut Context<Reader>) -> AnyElement {
    let mut shape = div().flex().flex_col().gap(ctx.measure.space(Space::Base));
    if page.identity.family == crate::model::pages::KindFamily::Callable {
        if let Some(pipe) = page.signature.known().and_then(|s| facet::semantics::recorded::callable(&s.text, &page.identity.name,
            facet::semantics::recorded::Language::from_name(page.identity.language.name()))) {
            shape = shape.child(facet::anatomy::pipe("indexed-shape", pipe, &ctx.measure, &facet::anatomy::Links::plain()));
        } else {
            let message = if page.signature.known().is_some() {
                "Semantic preview unavailable for this recorded signature. Open Code for the exact declaration."
            } else {
                "Input and result details were not captured in this index."
            };
            shape = shape.child(quiet(ctx.say(message), &ctx.measure, ctx.palette));
        }
    } else if let Some(members) = page.members.known() {
        let label = if page.identity.kind == Some(backend_library::DeclarationKind::Enum) { "One of" } else { "Parts" };
        if !members.made_of.is_empty() {
            shape = shape.child(text(ty::SMALL, &ctx.measure, ctx.palette.ink3).child(ctx.say(label)));
            for member in members.made_of.iter().take(24) { shape = shape.child(member_row(member, package, ctx, hover, cx)); }
            if members.made_of.len() > 24 { shape = shape.child(discovery::graph_control(format!("Explore {} more parts in graph", members.made_of.len()-24), ctx)); }
        } else {
            shape = shape.child(text(ty::SMALL, &ctx.measure, ctx.palette.ink3).child(ctx.say("No parts are recorded in this index.")));
        }
    } else {
        let words = page.members.gap().map(gap_words).unwrap_or_default();
        shape = shape.child(quiet(ctx.say(words), &ctx.measure, ctx.palette));
    }
    ctx.targets.track(Section::Shape.anchor(), shape).into_any_element()
}

fn capabilities(page: &SymbolPage, caps: &[facet::semantics::caps::Cap], ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> AnyElement {
    const USUAL: [&str; 13] = ["Copy", "Clone", "Debug", "Display", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash", "Default", "ToString", "ToOwned", "Borrow"];
    let (usual, special): (Vec<_>, Vec<_>) = caps.iter().cloned().partition(|cap| USUAL.contains(&cap.trait_name.as_ref()));
    let mut column = div().flex().flex_col().gap(ctx.measure.space(Space::Base));
    if !special.is_empty() { column = column.child(facet::anatomy::can("symbol-special-capabilities", special, &ctx.measure).bare()); }
    if !usual.is_empty() {
        let fold = SymbolFold::Capabilities;
        let open = ctx.symbol_disclosure.is_open(&fold);
        let motion = ctx.symbol_disclosure.unroll(fold.clone());
        column = column.child(fold_control(page, fold, format!("The usual {} capabilities", usual.len()), ctx, cx))
            .child(facet::anatomy::unroll::unroll("symbol-usual-capabilities", open, motion,
                facet::anatomy::can("symbol-usual-capabilities-body", usual, &ctx.measure).bare()));
    }
    column.into_any_element()
}

fn documented_failures(page: &SymbolPage, ctx: &mut Ctx<'_>) -> Option<AnyElement> {
    let mut rows = page.sections.sections.iter().filter(|section| presentation::is_failure(section.kind))
        .map(|section| (None, section)).collect::<Vec<_>>();
    if let Some(members) = page.members.known() {
        rows.extend(members.all().flat_map(|member| member.sections.sections.iter().filter(|s| presentation::is_failure(s.kind)).map(move |s| (Some(member.decl.name.as_ref()), s))));
    }
    if rows.is_empty() { return None; }
    let mut column = div().flex().flex_col().gap(ctx.measure.space(Space::Roomy)).child(section_heading(Section::Failure, ctx));
    for (index, (member, section)) in rows.into_iter().take(24).enumerate() {
        let (label, icon) = match section.kind {
            crate::model::pages::SectionKind::Errors => ("Failure", Icon::Alert),
            crate::model::pages::SectionKind::Panics => ("Panic", Icon::Zap),
            _ => ("Caller obligations", Icon::ShieldCheck),
        };
        let mut row = div().flex().flex_col().gap(ctx.measure.space(Space::Base));
        let label = member.map_or_else(|| label.to_owned(), |member| format!("{label} · {member}"));
        let words = presentation::failure_words(section);
        ctx.say(plain_markup(&words));
        row = row.child(div().flex().items_center().gap(ctx.measure.space(Space::Base))
            .child(icons::ui(icon, IconSize::S16, ctx.palette.coral.base).size(ctx.measure.icon(14.0)))
            .child(text(ty::SMALL, &ctx.measure, ctx.palette.ink1).child(ctx.say(label))))
            .child(facet::overlay::text::prose(format!("documented-failure-{index}"), words, ty::PROSE, &ctx.measure)
                .color(ctx.palette.ink1));
        column = column.child(row);
    }
    Some(column.into_any_element())
}

fn failures(page: &SymbolPage, anatomy: &crate::runtime::fixture_world::Anatomy, packet: &presentation::WorldPacket, ctx: &mut Ctx<'_>) -> Option<AnyElement> {
    let documented = documented_failures(page, ctx);
    if packet.failure.is_none() && !packet.callable_failure.as_ref().is_some_and(|f| f.all > 0) { return documented; }
    let mut column = div().flex().flex_col().gap(ctx.measure.space(Space::Roomy));
    if let Some(documented) = documented { column = column.child(documented); }
    else { column = column.child(section_heading(Section::Failure, ctx)); }
    let links = crate::runtime::fixture_world::links(&anatomy.world);
    if let Some(section) = &packet.failure {
        column = column.child(facet::anatomy::fails::fails("symbol-traced-failures", section.clone(), anatomy.world.clone(), &ctx.measure, &links));
    }
    if let Some(line) = packet.callable_failure.as_ref().filter(|f| f.all > 0) {
        column = column.child(text(ty::SMALL, &ctx.measure, ctx.palette.ink3).child(ctx.say(line.words(&anatomy.world))));
    }
    Some(column.into_any_element())
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

fn lens_bar(ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> Leaf {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut bar = div()
        .flex()
        .flex_wrap()
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

/// The Reference lens drawn from the world: the anatomy in place of the
/// declaration's code, the `can` line, Getting one / Calling it, the docs,
/// the plain relation list (the prism belongs to the graph), and Does in
/// place of the ledgers.
fn anatomy_reference(
    page: &SymbolPage,
    anatomy: &crate::runtime::fixture_world::Anatomy,
    packet: &presentation::WorldPacket,
    package: &str,
    ctx: &mut Ctx<'_>,
    hover: &mut HoverIntent,
    cx: &mut Context<Reader>,
) -> Vec<Leaf> {
    use facet::semantics::Word;
    use facet::semantics::page::Shape;
    let measure = ctx.measure;
    let links = crate::runtime::fixture_world::links(&anatomy.world);
    let mut leaves = Vec::new();
    let id = |part: &str| gpui::ElementId::Name(SharedString::from(format!("anatomy-{part}")));
    let shape = match anatomy.page.shape.clone() {
        Shape::Fork(fork) => facet::anatomy::fork(id("shape"), fork, &measure, &links).into_any_element(),
        Shape::Holds(holds) => facet::anatomy::holds(id("shape"), holds, &measure, &links).into_any_element(),
        Shape::Pipe(pipe) => facet::anatomy::pipe(id("shape"), pipe, &measure, &links).into_any_element(),
        Shape::Contract(contract) => facet::anatomy::contract(id("shape"), contract, &measure, &links).into_any_element(),
        Shape::Alias(ty) | Shape::Constant(ty) => {
            let mut line = facet::anatomy::Line::new();
            line.spelled(&ty, &facet::anatomy::TypeInk::new(facet::anatomy::roles::TYPE, ctx.palette), &links, ctx.reveal.xray);
            line.element(id("shape"), facet::anatomy::roles::TYPE, &measure, &links, ctx.palette).into_any_element()
        }
        Shape::None => indexed_shape(page, package, ctx, hover, cx),
    };
    leaves.push(Leaf::new(ctx.targets.track(Section::Shape.anchor(), shape)));
    if let Some(recipe) = &anatomy.recipe {
        let heading = section_heading(Section::Getting, ctx);
        let rail = facet::anatomy::recipe(id("recipe"), recipe.clone(), &measure, &links).without_title();
        let rail = if packet.relations.iter().any(|row| row.word == Word::MadeBy) { rail.without_foot() } else { rail };
        leaves.push(Leaf::new(div().flex().flex_col().child(heading)
            .child(rail)
            .child(discovery::world_rows(page, &anatomy.world, &packet.relations, &[Word::MadeBy], false, ctx, cx))));
    }
    if !anatomy.page.caps.is_empty() || !anatomy.page.does.is_empty() {
        let mut section = div().flex().flex_col().gap(measure.space(Space::Roomy)).child(section_heading(Section::Behavior, ctx));
        section = section.child(capabilities(page, &anatomy.page.caps, ctx, cx));
        let symbol = page.identity.coordinate.clone();
        let weak = cx.weak_entity();
        let expanded = facet::semantics::members::Receiver::ORDER.into_iter()
            .filter(|receiver| ctx.symbol_disclosure.is_open(&SymbolFold::Methods(index_receiver(*receiver)))).collect();
        section = section.child(facet::anatomy::does(id("does"), anatomy.page.does.clone(), &measure, &links)
            .without_title().disclosure(expanded, move |receiver, cx| {
                let _ = weak.update(cx, |reader, cx| reader.toggle_symbol(symbol.clone(), SymbolFold::Methods(index_receiver(receiver)), cx));
            }));
        leaves.push(Leaf::new(section));
    }
    if let Some(failures) = failures(page, anatomy, packet, ctx) { leaves.push(Leaf::new(failures)); }
    let omitted = [Word::MadeOf, Word::Takes, Word::Gives, Word::MadeBy];
    let has_connections = packet.relations.iter().any(|r| !omitted.contains(&r.word) && r.word != Word::Is);
    if has_connections || !anatomy.uses.is_empty() {
        let heading = section_heading(Section::Connections, ctx);
        let door = discovery::door(&packet.relations, ctx);
        let mut section = div().flex().flex_col().gap(measure.space(Space::Roomy))
            .child(div().flex().flex_wrap().items_center().justify_between().child(heading).child(door))
            .child(discovery::world_rows(page, &anatomy.world, &packet.relations, &omitted, true, ctx, cx));
        if !anatomy.uses.is_empty() {
            let fold = SymbolFold::Uses;
            let open = ctx.symbol_disclosure.is_open(&fold);
            let motion = ctx.symbol_disclosure.unroll(fold.clone());
            let uses = if open || !motion.is_empty() { facet::anatomy::in_use("anatomy-in-use", anatomy.uses.clone(), &measure, &links).into_any_element() } else { div().into_any_element() };
            let count = anatomy.uses.len();
            let label = if count == 1 { "1 example from a real caller".to_owned() } else { format!("{count} examples from real callers") };
            section = section.child(fold_control(page, fold, label, ctx, cx))
                .child(facet::anatomy::unroll::unroll("symbol-real-uses", open, motion, uses));
        }
        leaves.push(Leaf::new(section));
    }
    if let Some(docs) = docs(page, package, ctx) {
        leaves.push(Leaf::new(div().flex().flex_col().child(section_heading(Section::Documentation, ctx)).child(docs)));
    }
    leaves
}

fn index_receiver(receiver: facet::semantics::members::Receiver) -> Receiver {
    use facet::semantics::members::Receiver as R;
    match receiver { R::Reads => Receiver::Reads, R::Changes => Receiver::Changes, R::UsesUp => Receiver::Consumes, R::Makes => Receiver::Makes }
}

/// A relation group's verb (`taken by`, `held by`): UI face at ink3,
/// right-aligned in its own column and centred on the first row of names,
/// as the verb rows of the page targets set it.
fn verb(word: SharedString, measure: &Measure, palette: &Palette) -> gpui::Div {
    div()
        .w(px(104.0 * measure.scale()))
        .flex_none()
        .min_h(px(24.0 * measure.scale()))
        .flex()
        .items_center()
        .justify_end()
        .child(text(ty::SMALL, measure, palette.ink3).text_right().child(word))
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
                let page_number = if brief { 0 } else { ctx.symbol_disclosure.indexed_relation_page(label) };
                let (start, end) = relation_window(list.len(), page_number, brief);
                let limit = end - start;
                let mut names = div().flex().flex_wrap().gap(measure.space(Space::Base)).flex_1().min_w(px(0.0));
                for (index, relation) in list.iter().enumerate().skip(start).take(limit) {
                    names = names.child(relation_link(relation, package, label, index, ctx, cx));
                }
                if brief && list.len() > limit {
                    let more = ctx.say(format!("and {} more", list.len() - limit));
                    names = names.child(text(ty::SMALL, &measure, palette.ink3).child(more));
                }
                let mut content = div().flex().flex_col().gap(measure.space(Space::Tight)).flex_1().min_w(px(0.0)).child(names);
                if !brief && list.len() > 24 {
                    let count = ctx.say(format!("{}–{} of {} recorded", start + 1, end, list.len()));
                    let mut pager = div().flex().flex_wrap().items_center().gap(measure.space(Space::Roomy))
                        .child(text(ty::SMALL, &measure, palette.ink2).child(count));
                    if start > 0 {
                        pager = pager.child(fold_control(page, SymbolFold::IndexedRelationPage(label, start / 24 - 1), "Previous".to_owned(), ctx, cx));
                    }
                    if end < list.len() {
                        pager = pager.child(fold_control(page, SymbolFold::IndexedRelationPage(label, start / 24 + 1), "Next".to_owned(), ctx, cx));
                    }
                    content = content.child(pager);
                }
                let label = ctx.say(label);
                column = column.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(measure.space(Space::Roomy))
                        .child(verb(label, &measure, palette))
                        .child(content),
                );
            }
            Some(_) => {}
            None => {
                if !said_gap && let Some(gap) = relations.gap() {
                    said_gap = true;
                    // A fault is said in the UI face: serif is for the lede
                    // and the one sentence.
                    let words = ctx.say(gap_words(gap));
                    column = column.child(text(ty::SMALL, &measure, palette.ink3).child(words));
                }
            }
        }
    }
    column.into_any_element()
}

fn relation_window(len: usize, page: usize, brief: bool) -> (usize, usize) {
    if brief { return (0, len.min(6)); }
    if len == 0 { return (0, 0); }
    const PAGE: usize = 24;
    let start = page.min((len - 1) / PAGE) * PAGE;
    (start, start.saturating_add(PAGE).min(len))
}

fn relation_link(relation: &Relation, package: &str, direction: &str, index: usize, ctx: &mut Ctx<'_>, _cx: &mut Context<Reader>) -> AnyElement {
    let measure = ctx.measure;
    let palette = ctx.palette;
    let name = ctx.say(relation.decl.name.to_string());
    let target = relation.decl.coordinate.clone();
    let route = symbol_route(package, &target);
    let id: SharedString = format!("rel-{direction}-{index}-{}", target.as_str()).into();
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
                .min_h(px(24.0 * measure.scale()))
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
    if !members.does.is_empty() {
        let mut column = div().flex().flex_col().child(section_heading(Section::Behavior, ctx));
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
            let fold = SymbolFold::Methods(group.receiver);
            let open = ctx.symbol_disclosure.is_open(&fold);
            for member in group.members.iter().take(if open { 100 } else { 6 }) {
                column = column.child(member_row(member, package, ctx, hover, cx));
            }
            if group.members.len() > 6 {
                let label = if open { "Show fewer operations".to_owned() } else { format!("Explore {} more operations", group.members.len()-6) };
                column = column.child(fold_control(page, fold, label, ctx, cx));
                if open && group.members.len() > 100 { column = column.child(discovery::graph_control(format!("{} more operations in graph", group.members.len()-100), ctx)); }
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
        Receiver::Reads => "Inspect",
        Receiver::Changes => "Edit",
        Receiver::Consumes => "Take ownership",
        Receiver::Makes => "Associated operations",
        Receiver::Unknown => "Receiver not reported",
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
    let callable = member.signature.known().and_then(|signature| facet::semantics::recorded::callable(&signature.text, &name,
        facet::semantics::recorded::Language::from_name(member.decl.language.name())));
    let mut shape = div().flex().flex_col().gap(measure.space(Space::Tight));
    if let Some(pipe) = callable {
        let data = facet::anatomy::Operation { name: name.clone().into(), target: None,
            inputs: pipe.inputs.into_iter().filter_map(|input| input.ty).collect(), result: pipe.output, failure: pipe.fails, signature_known: true };
        shape = shape.child(facet::anatomy::operation(format!("member-ports-{}", member.decl.coordinate.as_str()), data, &measure, &facet::anatomy::Links::plain(), palette));
        ctx.say(name.clone());
    } else {
        let source = member.signature.known().map(|signature| signature.text.as_ref()).unwrap_or("");
        let scope = facet::semantics::types::Scope::new(&facet::semantics::types::Nowhere);
        let payload = source.strip_prefix(name.as_str()).map(str::trim).unwrap_or("");
        let mut line = facet::anatomy::Line::new();
        line.push(&name, facet::anatomy::roles::NAME, palette.ink0.hsla());
        if let Some(types) = payload.strip_prefix('(').and_then(|p| p.strip_suffix(')')) {
            for ty in facet::semantics::types::parse_list(types) {
                line.push(" · ", facet::anatomy::roles::TYPE, palette.ink3.hsla());
                line.spelled(&scope.spell(&ty), &facet::anatomy::TypeInk::new(facet::anatomy::roles::TYPE, palette), &facet::anatomy::Links::plain(), ctx.reveal.xray);
            }
        } else if let Some(ty) = payload.strip_prefix(':') {
            line.push(" · ", facet::anatomy::roles::TYPE, palette.ink3.hsla());
            line.spelled(&scope.spell_text(ty.trim()), &facet::anatomy::TypeInk::new(facet::anatomy::roles::TYPE, palette), &facet::anatomy::Links::plain(), ctx.reveal.xray);
        }
        ctx.say(line.text().to_owned());
        shape = shape.child(line.element(format!("member-parts-{}", member.decl.coordinate.as_str()), facet::anatomy::roles::NAME, &measure, &facet::anatomy::Links::plain(), palette));
    }
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
    let row = div()
        .id(id.clone())
        .flex()
        .items_start()
        .gap(measure.space(Space::Base))
        .min_h(measure.row() + measure.space(Space::Base))
        .px(measure.space(Space::Base))
        .hover(|style| style.bg(palette.tint))
        .when_focused(focused, palette)
        .child(crate::shell::kit::kind_mark(kind_of(member.decl.kind), KindSize::Sm, &measure, palette))
        .child(shape.flex_1().min_w(px(0.0)))

        .children(facts.map(|facts| {
            text(ty::MONO_SMALL, &measure, palette.ink3)
                .flex_none()
                .ml_auto()
                .pl(measure.space(Space::Roomy))
                .child(facts)
        }))
        .on_click(move |_: &ClickEvent, window, cx| act(window, cx))
        .on_hover(cx.listener(move |reader, hovered: &bool, _, cx| reader.hover_link(warm.clone(), *hovered, cx)));
    let mut card = div().flex().flex_col().gap(measure.space(Space::Tight)).child(ctx.targets.track(id, row));
    if let Some(summary) = summary { card = card.child(text(ty::CAPTION, &measure, palette.ink3).child(summary)); }
    if !member.docs.is_empty() && member.docs.iter().any(|f| matches!(f, DocFragment::Text(text) if text.contains("\n"))) {
        let fold = SymbolFold::Member(member.decl.coordinate.as_str().to_owned());
        let open = ctx.symbol_disclosure.is_open(&fold);
        let motion = ctx.symbol_disclosure.unroll(fold.clone());
        card = card.child(fold_control_for_member(member, fold, open, ctx, cx))
            .child(facet::anatomy::unroll::unroll(format!("member-docs-{}", member.decl.coordinate.as_str()), open, motion,
                text(ty::PROSE, &measure, palette.ink1).child(DocFragment::plain_text(&member.docs))));
    }
    if let Some(notice) = member.decl.facts.deprecated() {
        let note = notice.note.as_deref().unwrap_or("The author marks this operation deprecated.");
        card = card.child(text(ty::SMALL, &measure, palette.ink1).child(ctx.say(format!("Deprecated: {note}"))));
    }
    card.into_any_element()
}

fn fold_control_for_member(member: &Member, fold: SymbolFold, open: bool, ctx: &mut Ctx<'_>, cx: &mut Context<Reader>) -> AnyElement {
    let id: SharedString = format!("member-docs-fold-{}", member.decl.coordinate.as_str()).into();
    let weak = cx.weak_entity();
    // The current route identifies the parent page, so child documentation
    // does not start an independent disclosure history.
    let act: Act = Rc::new(move |_, cx| { let _ = weak.update(cx, |reader, cx| reader.toggle_current_symbol(fold.clone(), cx)); });
    let label = ctx.say(if open { "Close documentation" } else { "Read full documentation" });
    if ctx.active { ctx.targets.push(Target { id: id.clone(), label: label.clone(), act: act.clone(), peek: None, source: None }); }
    ctx.targets.track(id.clone(), div().id(id).py(ctx.measure.space(Space::Base)).child(text(ty::SMALL, &ctx.measure, ctx.palette.ink1).child(label))
        .on_click(move |_, window, cx| act(window, cx))).into_any_element()
}

trait WhenFocused: Styled + Sized {
    fn when_focused(self, focused: bool, palette: &Palette) -> Self {
        if focused { self.bg(palette.tint) } else { self }
    }
}

impl<E: Styled> WhenFocused for E {}

#[allow(dead_code)]
fn _icon(_: Icon, _: IconSize) {}

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

    #[test]
    fn indexed_relation_lens_shows_one_bounded_page_at_a_time() {
        assert_eq!(relation_window(10_000, 0, false), (0, 24));
        assert_eq!(relation_window(10_000, 1, false), (24, 48));
        assert_eq!(relation_window(10_000, 999_999, false), (9_984, 10_000));
        assert_eq!(relation_window(10_000, 0, true), (0, 6));
        assert_eq!(relation_window(0, 0, false), (0, 0));
    }
}
