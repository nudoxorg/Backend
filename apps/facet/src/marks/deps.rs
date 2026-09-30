//! Dependencies as marks: each is a real link (it opens that package's
//! page), and its card says what *you* use it for.
//!
//! - **A link at rest.** A diamond and the name in mono: solid when
//!   required, hollow when optional, mint when the package is yours; an
//!   optional dependency that is off in your tree reads quieter. Hovering
//!   lights the name and underlines it in periwinkle.
//! - **Its card.** What it is for, from real usage (the phrase a usage rule
//!   derived, else "Mostly `A`, `B`" from the items used most, else an
//!   honest unknown); how many uses and through what; whether it is always
//!   needed or optional (and whether that feature is on); the requirement
//!   and what it resolved to; and what it means for your tree (yours, one
//!   of several versions, "costs nothing" to turn on, "it leaves with it").
//! - **The line** ([`dep_line`]). Yours first, then required, then optional,
//!   each alphabetical; as many as fit on one line, then "and N more",
//!   which opens in place (the rest are uncovered in reading order). At
//!   rest the line never wraps, so no name is ever left alone on a line.
//!   Dev and build dependencies wait under ⌥.

use super::card::{self, Activate, Content, k, text};
use super::glyph::{self, Diamond};
use super::semver::{self, list, plural, thousands, word};
use crate::measure::Measure;
use crate::motion;
use crate::paint::mix;
use crate::probe;
use crate::theme::ActiveFacet;
use crate::tokens::motion::GLIDE;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, px,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// Which table the dependency is declared in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DepKind {
    /// `[dependencies]`.
    Normal,
    /// `[dev-dependencies]`: tests and examples.
    Dev,
    /// `[build-dependencies]`.
    Build,
}

/// Where the dependency stands in your lockfile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InTree {
    /// Every version of it your tree holds.
    pub versions: Vec<String>,
    /// Your packages that depend on it directly.
    pub yours: Vec<String>,
    /// What pulls it in (a sample).
    pub via: Vec<String>,
    /// How many packages pull it in, when more than `via` shows.
    pub via_count: Option<usize>,
    /// It is a package in your workspace.
    pub local: bool,
}

/// What a dependency link knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepFacts {
    /// Its name.
    pub name: SharedString,
    /// Which table declares it.
    pub kind: DepKind,
    /// The requirement as written (`0.22.27`, `path ../library`).
    pub req: Option<String>,
    /// What the lockfile resolved it to.
    pub resolved: Option<String>,
    /// The newest release, when there is no resolution to show.
    pub newest: Option<String>,
    /// Behind a feature.
    pub optional: bool,
    /// The features that turn it on.
    pub features: Vec<String>,
    /// Whether that feature is on by default.
    pub on_by_default: Option<bool>,
    /// Whether it is on in your tree (`false`: its lock edge is absent).
    pub on_in_tree: Option<bool>,
    /// How many times the package uses it; `None` when that is unknown
    /// (its uses are in files the package does not ship).
    pub uses: Option<usize>,
    /// What it uses most, with counts.
    pub items: Vec<(String, usize)>,
    /// What it is for, when a usage rule could say it.
    pub purpose: Option<String>,
    /// It is a package in your workspace.
    pub local: bool,
    /// Where it stands in your tree, when the reader has a lockfile.
    pub in_tree: Option<InTree>,
    /// What to say about your tree when there is no lockfile for it.
    pub tree_note: Option<String>,
    /// Where its link goes; `None` when the dependency never resolved to a
    /// package this app can open. Dead end #15 (`shell/kit.rs`): a link
    /// without a place is drawn as text, never as a control that looks
    /// live but goes nowhere, so `None` here draws the name plain, with no
    /// underline, no click, and no card.
    pub target: Option<SharedString>,
}

impl DepFacts {
    /// Yours (a workspace package).
    #[must_use]
    pub fn yours(&self) -> bool {
        self.local || self.in_tree.as_ref().is_some_and(|t| t.local)
    }

    /// Off in your tree: optional, and its feature is not on.
    #[must_use]
    pub fn off(&self) -> bool {
        self.optional && self.on_in_tree == Some(false)
    }

    /// The card's sentence: the usage phrase, else the items used most,
    /// else an honest unknown.
    #[must_use]
    pub fn sentence(&self) -> String {
        if let Some(purpose) = &self.purpose {
            return format!("{}.", capitalise(purpose));
        }
        let mut top: Vec<String> = Vec::new();
        for (item, _) in &self.items {
            let last = item.rsplit("::").next().unwrap_or(item).to_owned();
            if !top.contains(&last) {
                top.push(last);
            }
            if top.len() == 2 {
                break;
            }
        }
        match (self.uses, top.is_empty()) {
            (Some(_), false) => format!("Mostly {}.", top.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", ")),
            (None, _) => match self.kind {
                DepKind::Dev => "Dev-only: its uses are in files the package does not ship.".to_owned(),
                DepKind::Build => "Build-only: its uses are in its build script.".to_owned(),
                DepKind::Normal => "What it is used for is not known yet.".to_owned(),
            },
            (Some(0), true) => "Declared, but no use of it was found.".to_owned(),
            (Some(_), true) => "Used, but through nothing it names.".to_owned(),
        }
    }

    /// What the card says when its uses are not known, and why they are
    /// not (`None` when they are known). Until usage is supplied this is
    /// the line most dependency cards carry, so it must never go blank.
    #[must_use]
    pub const fn unknown_uses(&self) -> Option<&'static str> {
        if self.uses.is_some() {
            return None;
        }
        Some(match self.kind {
            DepKind::Normal => "uses unknown: not read yet",
            DepKind::Dev => "uses unknown: its tests are not shipped",
            DepKind::Build => "uses unknown: its build script is not read",
        })
    }

    /// When it is needed (`always`, `optional · on by default · …`).
    #[must_use]
    pub fn needed(&self) -> String {
        match self.kind {
            DepKind::Dev => "for tests and examples only".to_owned(),
            DepKind::Build => "at build time only".to_owned(),
            DepKind::Normal if !self.optional => "always".to_owned(),
            DepKind::Normal => {
                let state = match (self.on_in_tree, self.on_by_default) {
                    (Some(false), _) => "off in your tree",
                    (_, Some(true)) => "on by default",
                    (_, Some(false)) => "off by default",
                    (_, None) => "optional",
                };
                let features = if self.features.is_empty() { self.name.to_string() } else { self.features.join(", ") };
                format!("optional · {state} · {features}")
            }
        }
    }

    /// The version row (`^0.22.27 → 0.22.27`), when anything is known.
    #[must_use]
    pub fn version(&self) -> Option<String> {
        let req = self.req.as_ref().map(|r| if r.starts_with(|c: char| c.is_ascii_digit()) { format!("^{r}") } else { r.clone() });
        let got = self
            .resolved
            .as_ref()
            .map(|v| format!(" → {}", semver::short(v)))
            .or_else(|| self.newest.as_ref().map(|v| format!(" · newest {v}")));
        match (req, got) {
            (None, None) => None,
            (req, got) => Some(format!("{}{}", req.unwrap_or_default(), got.unwrap_or_default()).trim().to_owned()),
        }
    }

    /// The your-tree row, from `parent`'s point of view.
    #[must_use]
    pub fn tree(&self, parent: &str) -> String {
        if self.yours() {
            return "yours, in this workspace".to_owned();
        }
        let Some(it) = &self.in_tree else {
            return self.tree_note.clone().unwrap_or_else(|| "not in your tree".to_owned());
        };
        if self.off() {
            let feature = self.features.first().cloned().unwrap_or_else(|| self.name.to_string());
            let here = it.versions.last().map(|v| semver::short(v).to_owned()).unwrap_or_default();
            return format!("turning on {feature} costs nothing: {} {here} is already here", self.name);
        }
        if !it.yours.is_empty() {
            return format!("{} use it too", plural(it.yours.len(), "of your packages", "of your packages"));
        }
        if it.versions.len() > 1 {
            let versions: Vec<&str> = it.versions.iter().map(|v| semver::short(v)).collect();
            return format!("{} versions: {}", word(it.versions.len()), versions.join(", "));
        }
        let others: Vec<&String> = it.via.iter().filter(|x| x.as_str() != parent).collect();
        let total = it.via_count.unwrap_or(it.via.len()).saturating_sub(it.via.len() - others.len());
        if others.is_empty() {
            return format!("only through {parent}: it leaves with it");
        }
        let names: Vec<String> = others.iter().take(2).map(|s| (*s).clone()).collect();
        let who = if total > 2 { format!("{} and {} more", names.join(", "), total - 2) } else { list(&names, "and") };
        format!("also through {who}: it stays if {parent} goes")
    }
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().collect::<String>() + chars.as_str())
}

type Open = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// One dependency link (see the module docs).
#[derive(IntoElement)]
pub struct DepLink {
    id: ElementId,
    facts: Rc<DepFacts>,
    parent: SharedString,
    measure: Measure,
    on_open: Option<Open>,
    sheet: Option<u64>,
}

/// A link to `facts`, a dependency of `parent`.
#[must_use]
pub fn dep_link(id: impl Into<ElementId>, facts: DepFacts, parent: impl Into<SharedString>, measure: &Measure) -> DepLink {
    DepLink {
        id: id.into(),
        facts: Rc::new(facts),
        parent: parent.into(),
        measure: *measure,
        on_open: None,
        sheet: None,
    }
}

impl DepLink {
    /// Where following the link goes: called with its target.
    #[must_use]
    pub fn on_open(mut self, open: impl Fn(&SharedString, &mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(open));
        self
    }

    /// Opens the card on the first paint (state sheets, stills).
    #[must_use]
    pub const fn open_look(mut self) -> Self {
        self.sheet = Some(0);
        self
    }

    /// Opens the card `after` ms into the scene (films).
    #[must_use]
    pub const fn open_at(mut self, after: u64) -> Self {
        self.sheet = Some(after);
        self
    }
}

fn diamond_kind(facts: &DepFacts) -> Diamond {
    if facts.kind != DepKind::Normal {
        Diamond::Dev
    } else if facts.optional {
        Diamond::Hollow
    } else {
        Diamond::Solid
    }
}

fn diamond_ink(facts: &DepFacts, palette: &crate::tokens::Palette) -> Hsla {
    if facts.yours() {
        palette.mint.base.into()
    } else if facts.kind != DepKind::Normal {
        palette.ink4.into()
    } else {
        palette.ink3.into()
    }
}

fn diamond_element(kind: Diamond, size: f32, ink: Hsla) -> AnyElement {
    canvas(|_, _, _| {}, move |bounds, (), window, _| glyph::diamond(kind, bounds, ink, window))
        .size(px(size))
        .flex_none()
        .into_any_element()
}

impl RenderOnce for DepLink {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let measure = self.measure;
        let s = measure.scale();
        let key = ElementId::NamedChild(Arc::new(self.id.clone()), "card".into());
        let live = card::live(&self.id, &key, window, cx);
        let diamond = diamond_element(diamond_kind(&self.facts), 6.5 * s, diamond_ink(&self.facts, palette));
        let name_key = ElementId::NamedChild(Arc::new(self.id.clone()), "name".into());
        let Some(target) = self.facts.target.clone() else {
            // Dead end #15 (`shell/kit.rs`): a link without a place is
            // drawn as text, never as a control that looks live but goes
            // nowhere: no underline, no click, no card.
            let name = text(name_key, self.facts.name.clone(), card::DEP, &measure, palette.ink3);
            let row = div().flex().items_center().gap(px(7.0 * s)).child(diamond).child(name);
            return card::door(&self.id, &key, &live, None, self.sheet, None, row);
        };
        let quiet = self.facts.off() || self.facts.kind != DepKind::Normal;
        let rest: Hsla = if quiet { palette.ink3.into() } else { palette.ink1.into() };
        let ink = mix(rest, palette.ink0.into(), live.lit);
        let underline = crate::controls::with_alpha(palette.peri.base.into(), live.lit);
        let name = div()
            .border_b_1()
            .border_color(underline)
            .child(text(name_key, self.facts.name.clone(), card::DEP, &measure, ink));
        let open = self.on_open.clone();
        let click_target = target.clone();
        // A link is a target a finger or pointer can hit: at least 24 px
        // tall, its words centred in it.
        let link = div()
            .id(ElementId::NamedChild(Arc::new(self.id.clone()), "link".into()))
            .flex()
            .items_center()
            .min_h(px(24.0 * s))
            .gap(px(7.0 * s))
            .cursor_pointer()
            .child(diamond)
            .child(name)
            .on_click(move |_: &ClickEvent, window, cx| {
                if let Some(open) = &open {
                    open(&click_target, window, cx);
                }
            });
        let activate: Option<Activate> = self.on_open.clone().map(|open| {
            let target = target.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| open(&target, window, cx)) as Activate
        });
        let content = dep_card(self.facts.clone(), self.parent.clone());
        card::door(&self.id, &key, &live, Some(content), self.sheet, activate, link)
    }
}

fn dep_card(facts: Rc<DepFacts>, parent: SharedString) -> Content {
    Rc::new(move |measure: &Measure, _window: &mut Window, cx: &mut App| {
        let palette = cx.facet().palette();
        let s = measure.scale();
        let head = div()
            .flex()
            .items_center()
            .gap(k(measure, 8.0))
            .child(diamond_element(diamond_kind(&facts), 6.5 * s, diamond_ink(&facts, palette)))
            .child(div().flex_1().min_w_0().child(text("mk-dep-title", facts.name.clone(), card::TITLE_MONO, measure, palette.ink0)))
            .children(facts.resolved.as_ref().map(|v| text("mk-dep-v", semver::short(v).to_owned(), card::CODE, measure, palette.ink3)));
        let mut body = card::body(348.0, measure).child(head);
        body = body.child(div().mt(k(measure, 6.0)).child(sentence(&facts.sentence(), measure, palette)));
        body = body.child(uses(&facts, measure, palette));
        let mut rows: Vec<(&'static str, String)> = vec![("needed", facts.needed())];
        if let Some(version) = facts.version() {
            rows.push(("version", version));
        }
        rows.push(("your tree", facts.tree(&parent)));
        let mut kv = div()
            .mt(k(measure, 10.0))
            .pt(k(measure, 9.0))
            .border_t_1()
            .border_color(palette.line1.hsla())
            .flex()
            .flex_col()
            .gap(k(measure, 5.0));
        for (i, (key, value)) in rows.into_iter().enumerate() {
            let mint = value.contains("costs nothing") || value.starts_with("yours");
            kv = kv.child(
                div()
                    .flex()
                    .items_start()
                    .gap(k(measure, 14.0))
                    .child(div().w(k(measure, 62.0)).flex_none().child(text(("mk-dep-k", i), key, card::FACT, measure, palette.ink3)))
                    .child(div().flex_1().min_w_0().child(text(
                        ("mk-dep-kv", i),
                        value,
                        card::FACT,
                        measure,
                        if mint { palette.mint.base } else { palette.ink1 },
                    ))),
            );
        }
        body.child(kv).into_any_element()
    })
}

/// The sentence, with `code` spans in mono.
fn sentence(markup: &str, measure: &Measure, palette: &'static crate::tokens::Palette) -> AnyElement {
    let plain: String = markup.replace('`', "");
    probe::text(
        "mk-dep-say",
        plain,
        measure.role(card::SAY),
        1.0,
        probe::TextOverflow::Wrap,
        crate::overlay::text::prose("mk-dep-prose", markup.to_owned(), card::SAY, measure)
            .color(palette.ink1)
            .code_color(palette.ink2),
    )
    .into_any_element()
}

/// "**88** uses · most through `ser::ValueSerializer` 58".
fn uses(facts: &DepFacts, measure: &Measure, palette: &'static crate::tokens::Palette) -> AnyElement {
    let row = div().mt(k(measure, 7.0)).flex().flex_wrap().items_baseline().gap(k(measure, 5.0));
    let Some(n) = facts.uses else {
        let words = facts.unknown_uses().unwrap_or_default();
        return row.child(text("mk-dep-uses", words, card::FACT, measure, palette.ink3)).into_any_element();
    };
    let mut row = row
        .child(text("mk-dep-n", thousands(n), card::FACT_NUM, measure, palette.ink0))
        .child(text("mk-dep-unit", if n == 1 { "use" } else { "uses" }, card::FACT, measure, palette.ink2));
    if let Some((item, count)) = facts.items.first() {
        let more = facts.items.len() > 1 && *count < n;
        row = row.child(text("mk-dep-dot", "·", card::FACT, measure, palette.ink4));
        if more {
            row = row.child(text("mk-dep-through", "most through", card::FACT, measure, palette.ink2));
        }
        row = row.child(text("mk-dep-item", item.clone(), card::CODE, measure, palette.ink1));
        if more {
            row = row.child(text("mk-dep-count", count.to_string(), card::FACT, measure, palette.ink2));
        }
    }
    row.into_any_element()
}

/// The dependency line: "on", then links, folded to one line.
#[derive(IntoElement)]
pub struct DepLine {
    id: ElementId,
    deps: Rc<[DepFacts]>,
    parent: SharedString,
    measure: Measure,
    on_open: Option<Open>,
    open_look: bool,
    card_look: Vec<(SharedString, u64)>,
    wrap: Option<Rc<dyn Fn(&DepFacts, AnyElement) -> AnyElement>>,
}

/// The dependency line of `parent` over `deps`, as wide as `measure`.
#[must_use]
pub fn dep_line(id: impl Into<ElementId>, deps: impl Into<Rc<[DepFacts]>>, parent: impl Into<SharedString>, measure: &Measure) -> DepLine {
    DepLine {
        id: id.into(),
        deps: deps.into(),
        parent: parent.into(),
        measure: *measure,
        on_open: None,
        open_look: false,
        card_look: Vec::new(),
        wrap: None,
    }
}

impl DepLine {
    /// The host's hook on each link that goes somewhere (one with a
    /// target): it gets the dependency and its link, so it can make the
    /// link a keyboard target. A name with no place is not a door.
    #[must_use]
    pub fn wrap(mut self, wrap: impl Fn(&DepFacts, AnyElement) -> AnyElement + 'static) -> Self {
        self.wrap = Some(Rc::new(wrap));
        self
    }

    /// Where following a link goes: called with its target.
    #[must_use]
    pub fn on_open(mut self, open: impl Fn(&SharedString, &mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(open));
        self
    }

    /// Shows the fold open (state sheets).
    #[must_use]
    pub const fn open_look(mut self) -> Self {
        self.open_look = true;
        self
    }

    /// Opens `name`'s card on the first paint (stills).
    #[must_use]
    pub fn card_look(mut self, name: impl Into<SharedString>) -> Self {
        self.card_look.push((name.into(), 0));
        self
    }

    /// Opens `name`'s card `after` ms into the scene (films).
    #[must_use]
    pub fn card_at(mut self, name: impl Into<SharedString>, after: u64) -> Self {
        self.card_look.push((name.into(), after));
        self
    }
}

/// The order a line shows its dependencies in: yours, then required, then
/// optional, each alphabetical; dev and build dependencies after (⌥ only).
#[must_use]
pub fn order(deps: &[DepFacts]) -> Vec<&DepFacts> {
    let mut out: Vec<&DepFacts> = deps.iter().collect();
    out.sort_by(|a, b| {
        let rank = |d: &DepFacts| (d.kind != DepKind::Normal, !d.yours(), d.optional);
        rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// How many of `widths` (each link's width) fit in `room` with `gap`
/// between them, leaving room for a fold token `fold(n_hidden)` wide when
/// any are hidden. At least none; all when everything fits.
#[must_use]
pub fn fold_at(widths: &[f32], room: f32, gap: f32, fold: impl Fn(usize) -> f32) -> usize {
    let total: f32 = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    if total <= room {
        return widths.len();
    }
    let mut used = 0.0;
    let mut shown = 0;
    for (i, w) in widths.iter().enumerate() {
        let next = used + if i > 0 { gap } else { 0.0 } + w;
        let hidden = widths.len() - (i + 1);
        let token = if hidden > 0 { gap + fold(hidden) } else { 0.0 };
        if next + token > room {
            break;
        }
        used = next;
        shown = i + 1;
    }
    shown
}

struct FoldState {
    opened: Rc<Cell<Option<Instant>>>,
}

impl RenderOnce for DepLine {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let measure = self.measure;
        let s = measure.scale();
        let xray = measure.reveal().xray;
        let ordered: Vec<&DepFacts> = order(&self.deps).into_iter().filter(|d| xray || d.kind == DepKind::Normal).collect();
        let state = window.use_keyed_state(ElementId::NamedChild(Arc::new(self.id.clone()), "fold".into()), cx, |_, _| FoldState {
            opened: Rc::new(Cell::new(None)),
        });
        let opened_cell = state.read(cx).opened.clone();
        if self.open_look && opened_cell.get().is_none() {
            // A sheet shows it open and settled.
            opened_cell.set(Some(motion::now(cx) - std::time::Duration::from_secs(5)));
        }
        let gap = 18.0 * s;
        let dep_role = measure.role(card::DEP);
        let word_role = measure.role(card::WORD);
        let widths: Vec<f32> = ordered
            .iter()
            .map(|d| 6.5 * s + 7.0 * s + f32::from(probe::natural_width(&d.name, dep_role, 1.0, window)))
            .collect();
        let on_w = f32::from(probe::natural_width(&SharedString::new_static("on"), word_role, 1.0, window)) + 10.0 * s;
        let fold_words = |n: usize| format!("and {n} more");
        let fold_w = |n: usize| f32::from(probe::natural_width(&SharedString::from(fold_words(n)), word_role, 1.0, window));
        let room = f32::from(measure.width()) - on_w;
        let shown = fold_at(&widths, room, gap, fold_w);
        let hidden = ordered.len() - shown;
        let now = motion::now(cx);
        let open_t = opened_cell.get().map(|at| now.saturating_duration_since(at).as_secs_f32() * 1000.0);
        let open = open_t.is_some();
        let mut line = div()
            .id(self.id.clone())
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(gap))
            .gap_y(px(6.0 * s))
            .child(div().mr(px(10.0 * s - gap)).child(text(
                ElementId::NamedChild(Arc::new(self.id.clone()), "on".into()),
                "on",
                card::WORD,
                &measure,
                palette.ink3,
            )));
        if !open {
            // Nothing wraps at rest: the line folds instead.
            line = line.flex_nowrap().overflow_hidden();
        }
        let count = if open { ordered.len() } else { shown };
        let mut live = false;
        for (i, dep) in ordered.iter().take(count).enumerate() {
            let mut link = dep_link(
                ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(format!("dep-{}-{}", dep.name, i))),
                (*dep).clone(),
                self.parent.clone(),
                &measure,
            );
            if let Some(open) = self.on_open.clone() {
                link = link.on_open(move |target, window, cx| open(target, window, cx));
            }
            if let Some((_, after)) = self.card_look.iter().find(|(name, _)| *name == dep.name) {
                link = link.open_at(*after);
            }
            let link = match &self.wrap {
                Some(wrap) if dep.target.is_some() => wrap(dep, link.into_any_element()),
                _ => link.into_any_element(),
            };
            let element = if i >= shown {
                // Uncovered in reading order: 140 ms each, 20 ms apart.
                let t = open_t.unwrap_or(0.0) - 20.0 * (i - shown) as f32;
                let u = if motion::reduced(cx) { 1.0 } else { GLIDE.ease((t / 140.0).clamp(0.0, 1.0)) };
                live |= u < 1.0;
                motion::reveal(link).right(1.0 - u).into_any_element()
            } else {
                link
            };
            line = line.child(element);
        }
        if hidden > 0 && !open {
            let opened = opened_cell.clone();
            line = line.child(
                div()
                    .id(ElementId::NamedChild(Arc::new(self.id.clone()), "more".into()))
                    .cursor_pointer()
                    .child(text(
                        ElementId::NamedChild(Arc::new(self.id.clone()), "more-words".into()),
                        fold_words(hidden),
                        card::WORD,
                        &measure,
                        palette.ink3,
                    ))
                    .on_click(move |_: &ClickEvent, window, cx| {
                        opened.set(Some(motion::now(cx)));
                        window.refresh();
                    }),
            );
        }
        if live {
            motion::request_frame(window, cx);
        }
        line.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{DepFacts, DepKind, fold_at};

    fn unread(kind: DepKind) -> DepFacts {
        DepFacts {
            name: "unread".into(),
            kind,
            req: None,
            resolved: None,
            newest: None,
            optional: false,
            features: Vec::new(),
            on_by_default: None,
            on_in_tree: None,
            uses: None,
            items: Vec::new(),
            purpose: None,
            local: false,
            in_tree: None,
            tree_note: None,
            target: Some("crates.io/unread".into()),
        }
    }

    #[test]
    fn unknown_uses_say_why_for_every_kind_and_never_go_blank() {
        assert_eq!(unread(DepKind::Normal).unknown_uses(), Some("uses unknown: not read yet"));
        assert_eq!(unread(DepKind::Dev).unknown_uses(), Some("uses unknown: its tests are not shipped"));
        assert_eq!(unread(DepKind::Build).unknown_uses(), Some("uses unknown: its build script is not read"));
        let known = DepFacts { uses: Some(3), ..unread(DepKind::Normal) };
        assert_eq!(known.unknown_uses(), None, "known uses are counted, not excused");
    }

    #[test]
    fn the_fold_keeps_room_for_its_own_words() {
        // Four links of 100 px, 10 px apart, in 330 px with a 50 px token:
        // three would fit alone (320) but not with the token (380), so two.
        assert_eq!(fold_at(&[100.0; 4], 330.0, 10.0, |_| 50.0), 2);
        // Everything fits: no token at all.
        assert_eq!(fold_at(&[100.0; 3], 320.0, 10.0, |_| 50.0), 3);
        assert_eq!(fold_at(&[400.0], 300.0, 10.0, |_| 50.0), 0);
    }
}

/// A dependency card's content on its own (boards show it in place).
#[cfg(feature = "gallery")]
pub(crate) fn board_card(facts: &DepFacts, parent: &str) -> Content {
    dep_card(Rc::new(facts.clone()), parent.to_owned().into())
}
