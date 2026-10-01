//! The Library: a project's tree, grouped by the role each dependency plays.
//!
//! One thing speaks: the project's name and its one sentence ("Your 44
//! packages lean on 75 others directly, and 884 in all."). Under it, what
//! needs you (an unmaintained crate, in amber), then the roles in one or two
//! columns, then the packages that are here twice. Rows at rest are a mark, a
//! name and at most one quiet descriptor; why a dependency plays its role
//! waits in a tip, and how a duplicate got here unfolds in place.
//!
//! The model is plain words: the product spells every sentence
//! (`backend-present`), so the page shows what the CLI prints.

use crate::icons::{Icon, IconSize, Kind, KindSize, kind_mark, ui};
use crate::controls::button::button;
use crate::fluid::Modes;
use crate::motion::Flow;
use crate::measure::{Measure, Set, Space};
use crate::overlay::float::{self, FloatKind, FloatRequest};
use crate::overlay::tooltip::Tipped;
use crate::probe::{self, TextOverflow};
use crate::theme::ActiveFacet;
use crate::tokens::fluid::ROLES;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    AnyElement, App, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, div,
};
use std::sync::Arc;
use std::rc::Rc;

/// One source-backed release that a dependency row can offer.
#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseLink {
    /// Exact version as the owner recorded it.
    pub version: SharedString,
    /// An admitted package coordinate when this source can be opened.
    pub target: Option<SharedString>,
    /// Explicit reason when the tree cannot open this source.
    pub unavailable: Option<SharedString>,
}

/// Navigation supplied by the desktop; the shared facet never parses paths.
#[derive(Clone)]
pub struct Actions {
    pub open_package: Rc<dyn Fn(SharedString, &mut Window, &mut App)>,
}

/// How loudly an alert speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    /// A state to know about: unmaintained, a notice (amber).
    Warn,
    /// A fault: a vulnerability, malware, unsoundness (coral).
    Fault,
}

/// One advisory that affects the tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    /// "bincode 1.3.3 is unmaintained".
    pub title: SharedString,
    /// The advisory (`RUSTSEC-2025-0141`) and its own title, for the card.
    pub advisory: SharedString,
    /// How it got into the tree, for the card.
    pub path: SharedString,
    /// How loudly it speaks.
    pub tone: Tone,
}

/// One direct dependency.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Package name.
    pub name: SharedString,
    /// The one quiet descriptor at rest ("tests only", "twice · 0.22.1 · 0.23.1").
    pub at_rest: Option<SharedString>,
    /// Why it plays its role and who uses it, for the card rested open.
    pub why: SharedString,
    /// Its own one-line description, the card's one sentence.
    pub about: Option<SharedString>,
    /// Source-backed destinations for every resolved version.
    pub releases: Vec<ReleaseLink>,
}

/// One role and its dependencies.
#[derive(Clone, Debug, PartialEq)]
pub struct Role {
    /// "speaks formats".
    pub label: SharedString,
    /// "for engine, store and advisory".
    pub serving: Option<SharedString>,
    /// Its direct dependencies.
    pub rows: Vec<Row>,
    /// "and 15 crates that come with them".
    pub brings: Option<SharedString>,
}

/// A package present at more than one version.
#[derive(Clone, Debug, PartialEq)]
pub struct Twice {
    /// Package name.
    pub name: SharedString,
    /// Each copy's version, and whether it is yours.
    pub copies: Vec<(SharedString, bool)>,
    /// Each copy's path from your code.
    pub paths: Vec<SharedString>,
    /// What moving would do.
    pub verdict: SharedString,
}

/// Everything the Library shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    /// The project's name.
    pub name: SharedString,
    /// Its one sentence.
    pub lede: SharedString,
    /// Rested on the sentence: what builds only elsewhere.
    pub lede_tip: Option<SharedString>,
    /// How the tree was read, when not by Cargo for this machine.
    pub note: Option<SharedString>,
    /// What affects the tree.
    pub alerts: Vec<Alert>,
    /// Quiet facts after the alerts ("60 crates are here twice", advisory coverage).
    pub facts: Vec<SharedString>,
    /// The roles, in order.
    pub roles: Vec<Role>,
    /// "Here twice", then "60 crates appear at more than one version", when any do.
    pub twice_heading: Option<(SharedString, SharedString)>,
    /// The packages here twice, most actionable first.
    pub twice: Vec<Twice>,
}

/// How many duplicates show before "and N more".
pub const TWICE_AT_REST: usize = 8;
/// A single reveal adds a bounded run; large workspaces never mount every
/// dependency row because one disclosure was opened.
const REVEAL_BATCH: usize = 24;

/// The Library page for `model`, `measure` wide.
#[must_use]
pub fn library(id: impl Into<ElementId>, model: Arc<Model>, actions: Actions, measure: &Measure) -> Library {
    Library {
        id: id.into(),
        model,
        actions,
        measure: *measure,
    }
}

/// See [`library`].
#[derive(IntoElement)]
pub struct Library {
    id: ElementId,
    model: Arc<Model>,
    actions: Actions,
    measure: Measure,
}

/// A package mark at the size its row's text is set at (a 100 % mark beside
/// 200 % text is a speck).
fn package_mark(measure: &Measure, palette: &Palette) -> AnyElement {
    let (boxed, _, _) = KindSize::Sm.metrics();
    let wanted = boxed * measure.scale();
    let size = if wanted < 16.5 {
        KindSize::Sm
    } else if wanted < 22.0 {
        KindSize::Md
    } else {
        KindSize::Lg
    };
    kind_mark(Kind::Package, size, palette)
}

fn child(parent: &ElementId, part: impl Into<SharedString>) -> ElementId {
    ElementId::NamedChild(Arc::new(parent.clone()), part.into())
}

/// Words in one face, published for the layout and contrast lints.
fn words(
    key: ElementId,
    text: SharedString,
    role: TypeRole,
    color: impl Into<Hsla>,
    measure: &Measure,
    overflow: TextOverflow,
) -> AnyElement {
    let color: Hsla = color.into();
    let mut body = div().set(role, measure).text_color(color).child(text.clone());
    if overflow == TextOverflow::Ellipsis {
        body = body.overflow_hidden().whitespace_nowrap().text_ellipsis();
    }
    probe::text(key, text, measure.role(role), 1.0, overflow, body).into_any_element()
}

impl RenderOnce for Library {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let model = self.model;
        let id = self.id;
        let mut page = div()
            .id(id.clone())
            .flex()
            .flex_col()
            .w(measure.width())
            .gap(measure.space(Space::Wide))
            .child(hero(&id, &model, &measure, palette));
        if let Some(note) = &model.note {
            page = page.child(words(child(&id, "note"), note.clone(), ty::CAPTION, palette.ink2, &measure, TextOverflow::Wrap));
        }
        page = page.child(roles(&id, &model, &self.actions, &measure, palette, window, cx));
        page
    }
}

fn hero(id: &ElementId, model: &Model, measure: &Measure, palette: &Palette) -> AnyElement {
    let gem = measure.fluid(34.0, 44.0);
    let mut lede = div()
        .id(child(id, "lede"))
        .child(words(child(id, "lede-words"), model.lede.clone(), ty::LEDE, palette.ink2, measure, TextOverflow::Wrap));
    lede = lede.cursor_default();
    let lede: AnyElement = match &model.lede_tip {
        Some(tip) => lede.tip(tip.clone()).into_any_element(),
        None => lede.into_any_element(),
    };
    let mut facts = div().flex().flex_wrap().items_baseline().gap_x(measure.space(Space::Gutter)).gap_y(measure.space(Space::Tight));
    for (index, alert) in model.alerts.iter().enumerate() {
        let tone = match alert.tone {
            Tone::Warn => palette.amber.base,
            Tone::Fault => palette.coral.base,
        };
        let title = SharedString::from(format!("{} ›", alert.title));
        let trigger = div()
            .id(child(id, format!("alert-{index}")))
            .cursor_default()
            .child(words(child(id, format!("alert-words-{index}")), title, ty::SMALL, tone, measure, TextOverflow::Wrap));
        facts = facts.child(card_on_rest(
            child(id, format!("alert-card-{index}")),
            vec![
                (alert.advisory.clone(), ty::SMALL, Ink::Strong),
                (alert.path.clone(), ty::MONO_SMALL, Ink::Quiet),
            ],
            trigger,
        ));
    }
    for (index, fact) in model.facts.iter().enumerate() {
        let color = if index == 0 && model.alerts.is_empty() { palette.ink1 } else { palette.ink2 };
        facts = facts.child(words(child(id, format!("fact-{index}")), fact.clone(), ty::SMALL, color, measure, TextOverflow::Wrap));
    }
    div()
        .flex()
        .flex_col()
        .gap(measure.space(Space::Roomy))
        .pt(measure.space(Space::Wide))
        .child(
            div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Gutter))
                .child(crate::paint::gem(Kind::Module).size(f32::from(gem)))
                .child(words(child(id, "name"), model.name.clone(), ty::HERO, palette.ink0, measure, TextOverflow::Wrap)),
        )
        .child(lede)
        .child(facts)
        .into_any_element()
}

/// Roles in one or two columns, balanced by height, then "Here twice".
fn roles(id: &ElementId, model: &Model, actions: &Actions, measure: &Measure, palette: &Palette, window: &mut Window, cx: &mut App) -> AnyElement {
    let modes = Modes::keyed(child(id, "roles-modes"), window, cx);
    let laid = modes.columns(&ROLES, measure.fluid_room(), measure.space(Space::Section));
    let (count, column) = (laid.count, measure.within(laid.column.width()));
    // A change of column count is an epoch: the blocks spring to their new
    // places instead of jumping.
    let flow = Flow::scoped(format!("library-roles-{id:?}"), cx);
    flow.epoch((laid.epoch, count));
    // Each block's height in rows; the duplicates block goes last, in the shorter column.
    let mut blocks: Vec<(usize, AnyElement)> = model
        .roles
        .iter()
        .enumerate()
        .map(|(index, role)| {
            let block = role_block(id, index, role, actions, &column, palette, window, cx);
            (role.rows.len().min(6) + 2, flow.item(child(id, format!("role-flow-{index}")), block).into_any_element())
        })
        .collect();
    if !model.twice.is_empty() {
        let block = twice_block(id, model, &column, palette, window, cx);
        blocks.push((TWICE_AT_REST + 2, flow.item(child(id, "twice-flow"), block).into_any_element()));
    }
    let mut columns: Vec<(usize, Vec<AnyElement>)> = (0..count).map(|_| (0, Vec::new())).collect();
    let total: usize = blocks.iter().map(|(height, _)| *height).sum();
    let mut filled = 0;
    for (height, block) in blocks {
        // Fill the first column to half, then the rest: keeps reading order down each column.
        let target = if count > 1 && filled >= total.div_ceil(2) { 1 } else { 0 };
        filled += height;
        let slot = &mut columns[target.min(count - 1)];
        slot.0 += height;
        slot.1.push(block);
    }
    let mut row = div().flex().items_start().gap(measure.space(Space::Section));
    for (_, blocks) in columns {
        row = row.child(
            div()
                .flex()
                .flex_col()
                .w(column.width())
                .gap(measure.space(Space::Wide))
                .children(blocks),
        );
    }
    row.into_any_element()
}

fn role_block(id: &ElementId, index: usize, role: &Role, actions: &Actions, measure: &Measure, palette: &Palette, window: &mut Window, cx: &mut App) -> AnyElement {
    let block_id = child(id, format!("role-{index}"));
    let shown_count = window.use_keyed_state(child(&block_id, "shown"), cx, |_, _| 6usize);
    let icon = match role.label.as_ref() {
        "checks our work" => Icon::ShieldCheck, "draws the window" => Icon::Mosaic,
        "reads languages" => Icon::Book, "speaks formats" => Icon::Split,
        "keeps and finds" => Icon::Search, "runs things at once" => Icon::Zap,
        "talks to the network" => Icon::Globe, "fingerprints and packs" => Icon::Seal,
        "names what went wrong" => Icon::Alert, "watches itself run" => Icon::Eye,
        "shapes memory" => Icon::Diamond, "talks to the system" => Icon::Settings, _ => Icon::Layers,
    };
    let mut head = div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap_x(measure.space(Space::Roomy))
        .child(ui(icon, IconSize::S18, palette.ink2))
        .child(words(child(&block_id, "label"), role.label.clone(), ty::HEAD, palette.ink0, measure, TextOverflow::Wrap));
    if let Some(serving) = &role.serving {
        head = head.child(words(child(&block_id, "serving"), serving.clone(), ty::SMALL, palette.ink2, measure, TextOverflow::Wrap));
    }
    let mut block = div().flex().flex_col().gap(measure.space(Space::Tight)).child(head);
    let shown = role.rows.len().min(*shown_count.read(cx));
    for (at, row) in role.rows.iter().take(shown).enumerate() {
        block = block.child(row_view(&child(&block_id, format!("row-{at}")), row, actions, measure, palette, window, cx));
    }
    if role.rows.len() > shown {
        block = block.child(button(child(&block_id, "more"), format!("Show more · {} remain", role.rows.len() - shown), measure).ghost()
            .on_click(move |_, cx| shown_count.update(cx, |shown, cx| { *shown = shown.saturating_add(REVEAL_BATCH); cx.notify(); })));
    }
    if let Some(brings) = &role.brings {
        block = block.child(
            div()
                .pt(measure.space(Space::Tight))
                .child(words(child(&block_id, "brings"), brings.clone(), ty::SMALL, palette.ink2, measure, TextOverflow::Wrap)),
        );
    }
    block.into_any_element()
}

fn row_view(id: &ElementId, row: &Row, actions: &Actions, measure: &Measure, palette: &Palette, window: &mut Window, cx: &mut App) -> AnyElement {
    let expanded = window.use_keyed_state(child(id, "expanded"), cx, |_, _| false);
    let is_expanded = *expanded.read(cx);
    let toggle = expanded.clone();
    let mut line = div()
        .id(id.clone())
        .flex()
        .items_center()
        .gap(measure.space(Space::Snug))
        .h(measure.row())
        .px(measure.space(Space::Tight))
        .cursor_pointer()
        .hover(|style| style.bg(palette.tint))
        .child(package_mark(measure, palette))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(words(child(id, "name"), row.name.clone(), ty::MONO_ROW, palette.ink1, measure, TextOverflow::Ellipsis)),
        )
        .on_click(move |_, _, cx| toggle.update(cx, |expanded, cx| { *expanded = !*expanded; cx.notify(); }));
    if let Some(rest) = &row.at_rest {
        line = line.child(
            div()
                .flex_none()
                .child(words(child(id, "rest"), rest.clone(), ty::SMALL, palette.ink3, measure, TextOverflow::Wrap)),
        );
    }
    let mut lines = vec![(row.why.clone(), ty::SMALL, Ink::Strong)];
    if let Some(about) = &row.about {
        lines.push((about.clone(), ty::CAPTION, Ink::Quiet));
    }
    let trigger = card_on_rest(child(id, "card"), lines, line);
    if !is_expanded { return trigger; }
    let mut detail = div().flex().flex_col().gap(measure.space(Space::Tight))
        .pl(measure.space(Space::Wide)).pb(measure.space(Space::Base))
        .child(words(child(id, "why-inline"), row.why.clone(), ty::SMALL, palette.ink2, measure, TextOverflow::Wrap));
    if let Some(about) = &row.about { detail = detail.child(words(child(id, "about-inline"), about.clone(), ty::LEDE, palette.ink2, measure, TextOverflow::Wrap)); }
    for (at, release) in row.releases.iter().enumerate() {
        if let Some(target) = &release.target {
            let target = target.clone();
            let open = Rc::clone(&actions.open_package);
            detail = detail.child(button(child(id, format!("open-{at}")), format!("Open {} ›", release.version), measure)
                .ghost().on_click(move |window, cx| open(target.clone(), window, cx)));
        } else if let Some(reason) = &release.unavailable {
            detail = detail.child(words(child(id, format!("unavailable-{at}")),
                format!("{} · {reason}", release.version).into(), ty::CAPTION, palette.ink3, measure, TextOverflow::Wrap));
        }
    }
    div().flex().flex_col().child(trigger).child(detail).into_any_element()
}

/// How strongly a card line speaks.
#[derive(Clone, Copy)]
enum Ink {
    Strong,
    Quiet,
}

/// Rest on `trigger` and a small card opens with `lines`, each wrapping
/// inside the card's width (a tip's single line would cut them).
fn card_on_rest(
    key: ElementId,
    lines: Vec<(SharedString, TypeRole, Ink)>,
    trigger: impl IntoElement,
) -> AnyElement {
    let lines = Arc::new(lines);
    let request_key = key.clone();
    float::trigger(
        key,
        move |bounds| {
            let lines = Arc::clone(&lines);
            let card_key = request_key.clone();
            FloatRequest::new(request_key.clone(), bounds, FloatKind::Lens, move |measure, _window, cx| {
                let palette = cx.palette();
                let inner = measure.inset(measure.space(Space::Roomy));
                let mut card = div()
                    .flex()
                    .flex_col()
                    .gap(measure.space(Space::Tight))
                    .p(measure.space(Space::Roomy))
                    .max_w(measure.width());
                for (at, (text, role, ink)) in lines.iter().enumerate() {
                    let color = match ink {
                        Ink::Strong => palette.ink1,
                        Ink::Quiet => palette.ink3,
                    };
                    card = card.child(
                        div().w(inner.width()).child(words(
                            child(&card_key, format!("line-{at}")),
                            text.clone(),
                            *role,
                            color,
                            &inner,
                            TextOverflow::Wrap,
                        )),
                    );
                }
                card.into_any_element()
            })
        },
        trigger,
    )
    .into_any_element()
}

fn twice_block(id: &ElementId, model: &Model, measure: &Measure, palette: &Palette, window: &mut Window, cx: &mut App) -> AnyElement {
    let block_id = child(id, "twice");
    let shown_count = window.use_keyed_state(child(&block_id, "shown"), cx, |_, _| TWICE_AT_REST);
    let mut head = div().flex().flex_wrap().items_baseline().gap_x(measure.space(Space::Roomy));
    if let Some((title, caption)) = &model.twice_heading {
        head = head
            .child(words(child(&block_id, "label"), title.clone(), ty::HEAD, palette.ink0, measure, TextOverflow::Wrap))
            .child(words(child(&block_id, "caption"), caption.clone(), ty::SMALL, palette.ink2, measure, TextOverflow::Wrap));
    }
    let mut block = div().flex().flex_col().gap(measure.space(Space::Tight)).child(head);
    let shown = model.twice.len().min(*shown_count.read(cx));
    for (at, twice) in model.twice.iter().take(shown).enumerate() {
        block = block.child(twice_row(&child(&block_id, format!("row-{at}")), twice, measure, palette, window, cx));
    }
    let hidden = model.twice.len().saturating_sub(shown);
    if hidden > 0 {
        let label = SharedString::from(format!("and {hidden} more, deeper in the tree"));
        block = block.child(
            div()
                .id(child(&block_id, "more"))
                .pt(measure.space(Space::Tight))
                .cursor_pointer()
                .child(words(child(&block_id, "more-words"), label, ty::SMALL, palette.ink3, measure, TextOverflow::Wrap))
                .on_click(move |_, _, cx| shown_count.update(cx, |shown, cx| {
                    *shown = shown.saturating_add(REVEAL_BATCH);
                    cx.notify();
                })),
        );
    }
    block.into_any_element()
}

fn twice_row(id: &ElementId, twice: &Twice, measure: &Measure, palette: &Palette, window: &mut Window, cx: &mut App) -> AnyElement {
    let open = window.use_keyed_state(child(id, "open"), cx, |_, _| false);
    let is_open = *open.read(cx);
    let mut versions = div().flex().flex_none().items_baseline().gap(measure.space(Space::Snug));
    for (at, (version, yours)) in twice.copies.iter().enumerate() {
        if at > 0 {
            versions = versions.child(words(child(id, format!("dot-{at}")), "·".into(), ty::MONO_SMALL, palette.ink4, measure, TextOverflow::Clip));
        }
        let color = if *yours { palette.mint.base } else { palette.ink2 };
        versions = versions.child(words(child(id, format!("v-{at}")), version.clone(), ty::MONO_SMALL, color, measure, TextOverflow::Wrap));
    }
    let line = div()
        .id(id.clone())
        .flex()
        .items_center()
        .gap(measure.space(Space::Snug))
        .h(measure.row())
        .px(measure.space(Space::Tight))
        .cursor_pointer()
        .hover(|style| style.bg(palette.tint))
        .child(package_mark(measure, palette))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(words(child(id, "name"), twice.name.clone(), ty::MONO_ROW, if is_open { palette.ink0 } else { palette.ink1 }, measure, TextOverflow::Ellipsis)),
        )
        .child(versions)
        .on_click(move |_, _, cx| open.update(cx, |open, cx| {
            *open = !*open;
            cx.notify();
        }));
    let mut block = div().flex().flex_col().child(line);
    if is_open {
        let inset = measure.space(Space::Gutter) + measure.space(Space::Snug);
        let mut detail = div().flex().flex_col().gap(measure.space(Space::Tight)).pl(inset).pb(measure.space(Space::Snug));
        for (at, path) in twice.paths.iter().enumerate() {
            let yours = twice.copies.get(at).is_some_and(|(_, yours)| *yours);
            detail = detail.child(words(
                child(id, format!("path-{at}")),
                path.clone(),
                ty::MONO_SMALL,
                if yours { palette.mint.base } else { palette.ink2 },
                measure,
                TextOverflow::Wrap,
            ));
        }
        detail = detail.child(words(child(id, "verdict"), twice.verdict.clone(), ty::SMALL, palette.ink2, measure, TextOverflow::Wrap));
        block = block.child(detail);
    }
    block.into_any_element()
}
