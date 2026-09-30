//! The version mark: the version *is* the comb (`controls::comb`).
//!
//! - **Hero: the Rider.** The number is the pin's own tooth. Releases
//!   before it are history; after it they trail off fainter, breaking ones
//!   standing taller; yanked releases are hollow and pre-releases hatched;
//!   other versions your lockfile holds are hollow mint teeth; the newest is
//!   named faintly at the end.
//! - **Rows: the Baseline** ([`VersionMark::row`]): the number still at the
//!   start, the pin a mint tick inside.
//! - **Below 240 px: a band**, whichever was asked for.
//! - **The loved slider stays**: drag or ←/→ to scrub, Esc home; the number
//!   rides the scrub and only its changed semver wheels roll.
//! - **Cards.** The number's: where you stand ("28 releases behind · two of
//!   them breaking · 15 months"), what an upgrade does to *your* code when
//!   the index measured it (including "No API changes" when that is true),
//!   and the other copies your tree holds. Each also tooth's: who pulls that
//!   copy in and whether moving yours drops it.
//! - **Never published** (a workspace package): one mint tooth trailing a
//!   dashed "unpublished" line; its card says so.
//! - **Unknown** stays unknown: no release history is said as such.

use super::card::{self, Content, k, text};
use super::semver::{self, ReleaseFact, list, plural};
use crate::controls::comb::{AlsoTooth, CombStyle, Release, ReleaseId, Step, VersionSelected, version_comb};
use crate::fluid::Modes;
use crate::measure::Measure;
use crate::theme::ActiveFacet;
use crate::tokens::fluid::{COMB, Comb};
use gpui::{
    AnyElement, App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// Another version of the package your lockfile holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Also {
    /// The version.
    pub v: String,
    /// What pulls this copy in (a sample).
    pub via: Vec<String>,
    /// Your packages that pin this copy.
    pub yours: Vec<String>,
}

/// An API diff the index measured between your pin and a release.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    /// Public items added, removed and changed.
    pub added: usize,
    /// Removed.
    pub removed: usize,
    /// Changed.
    pub changed: usize,
    /// Your call sites whose meaning changes.
    pub your_sites_changed: usize,
    /// Your call sites that touch a changed item.
    pub your_sites_touched: usize,
    /// Items you use that are only respelled.
    pub respelled: Vec<String>,
}

impl Diff {
    /// Nothing public changed.
    #[must_use]
    pub const fn none(&self) -> bool {
        self.added == 0 && self.removed == 0 && self.changed == 0
    }
}

/// What the index measured about the package's API across releases.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Measured {
    /// Your uses of the package, when counted.
    pub your_uses: Option<usize>,
    /// Diffs from your pin to each measured release.
    pub diffs: Vec<(String, Diff)>,
    /// Public item counts per measured release.
    pub api_size: Vec<(String, usize)>,
}

impl Measured {
    fn diff(&self, v: &str) -> Option<&Diff> {
        self.diffs.iter().find(|(x, _)| x == v || semver::short(x) == semver::short(v)).map(|(_, d)| d)
    }

    fn size(&self, v: &str) -> Option<usize> {
        self.api_size.iter().find(|(x, _)| x == v || semver::short(x) == semver::short(v)).map(|(_, n)| *n)
    }
}

/// What the version mark knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionFacts {
    /// The package's name, for sentences.
    pub name: SharedString,
    /// Every release the registry knows (any order).
    pub releases: Vec<ReleaseFact>,
    /// The version your lockfile pins; `None`: not in your tree.
    pub pin: Option<String>,
    /// Other versions your lockfile holds.
    pub also: Vec<Also>,
    /// Your packages that pin `pin`.
    pub yours: Vec<String>,
    /// What pulls the pinned copy in, when none of it is yours.
    pub pin_via: Vec<String>,
    /// The API as the index measured it, when it did.
    pub measured: Option<Measured>,
    /// A workspace package that was never published: its path.
    pub local: Option<SharedString>,
    /// Today, for "ago" (pinned in fixtures).
    pub now: SharedString,
}

impl VersionFacts {
    fn sorted(&self) -> Vec<ReleaseFact> {
        let mut sorted = self.releases.clone();
        sorted.sort_by(|a, b| semver::cmp(&a.v, &b.v));
        sorted
    }

    /// The number card's lines: the head, the reading, what an upgrade does
    /// to your code, and the other copies your tree holds.
    #[must_use]
    pub fn number_lines(&self) -> NumberCard {
        let reading = semver::reading(&self.releases, self.pin.as_deref(), &self.now);
        let latest = reading.latest.clone();
        let head = match (&self.pin, &latest, reading.behind) {
            (Some(pin), Some(latest), Some(behind)) if behind > 0 => {
                (semver::short(pin).to_owned(), Some(semver::short(latest).to_owned()))
            }
            (Some(pin), _, _) => (semver::short(pin).to_owned(), None),
            (None, latest, _) => (latest.as_deref().map(semver::short).unwrap_or_default().to_owned(), None),
        };
        let mut yours = None;
        // What the index measured: the newest measured release after the
        // pin. When that is not the newest release, the card says how far
        // the measurement reaches instead of claiming the rest.
        let measured_after = self.measured.as_ref().and_then(|m| {
            let pin = self.pin.as_deref()?;
            m.diffs
                .iter()
                .filter(|(v, _)| semver::cmp(v, pin).is_gt())
                .max_by(|a, b| semver::cmp(&a.0, &b.0))
                .map(|(v, d)| (m, v.clone(), d))
        });
        if let (Some((m, through, diff)), Some(latest), Some(pin)) = (measured_after, &latest, &self.pin)
            && reading.behind.is_some_and(|b| b > 0)
        {
            let whole = semver::short(&through) == semver::short(latest);
            yours = Some(if diff.none() && whole {
                match m.size(latest).or_else(|| m.size(pin)) {
                    Some(n) => format!(
                        "No API changes: {} has the same {} as {}.",
                        semver::short(latest),
                        plural(n, "public item", "public items"),
                        semver::short(pin)
                    ),
                    None => format!("No API changes between {} and {}.", semver::short(pin), semver::short(latest)),
                }
            } else if diff.none() {
                format!(
                    "No API changes through {}; {} is not measured yet.",
                    semver::short(&through),
                    semver::short(latest)
                )
            } else if diff.your_sites_changed == 0 {
                let uses = m.your_uses.map_or_else(|| "your uses".to_owned(), |n| format!("your {n} uses"));
                let respelled = if diff.respelled.is_empty() {
                    String::new()
                } else {
                    let names: Vec<&str> = diff.respelled.iter().map(|p| p.rsplit("::").next().unwrap_or(p)).collect();
                    format!(
                        "; {} to {} {} respelled, not changed",
                        plural(diff.your_sites_touched, "call", "calls"),
                        names.join(", "),
                        if diff.your_sites_touched == 1 { "is" } else { "are" }
                    )
                };
                format!("None of {uses} change{respelled}.")
            } else {
                format!("{} change.", plural(diff.your_sites_changed, "of your uses", "of your uses"))
            });
        }
        let also: Vec<String> = self.also.iter().filter(|a| Some(&a.v) != self.pin.as_ref()).map(|a| semver::short(&a.v).to_owned()).collect();
        let dup = (!also.is_empty()).then(|| format!("Your tree also holds {}.", also.join(", ")));
        NumberCard {
            head,
            reading: reading.words,
            yours,
            dup,
        }
    }

    /// The card of the also tooth for `also`.
    #[must_use]
    pub fn also_lines(&self, also: &Also) -> (String, String) {
        let via = |x: &[String]| {
            if x.len() > 3 {
                format!("{} and {} more", x[..3].join(", "), x.len() - 3)
            } else {
                list(x, "and")
            }
        };
        let pin = self.pin.as_deref().map(semver::short).unwrap_or_default();
        let pin_side = if !self.yours.is_empty() {
            format!("your {} pin {pin}", plural(self.yours.len(), "package", "packages"))
        } else if !self.pin_via.is_empty() {
            format!("{pin} through {}", via(&self.pin_via))
        } else {
            format!("you pin {pin}")
        };
        let say = format!(
            "Two copies of {} compile. {} comes through {}; {pin_side}.",
            self.name,
            semver::short(&also.v),
            via(&also.via)
        );
        let older_is_also = self.pin.as_deref().is_some_and(|p| semver::cmp(&also.v, p).is_lt());
        let older: (&str, &[String]) = if older_is_also { (&also.v, &also.via) } else { (pin, &self.pin_via) };
        let fact = if self.yours.is_empty() && !self.pin_via.is_empty() {
            format!(
                "Neither is yours to move: {} still ask for {}.x.",
                plural(older.1.len(), "crate", "crates"),
                semver::short(older.0).split('.').next().unwrap_or_default()
            )
        } else {
            match self.measured.as_ref().and_then(|m| m.diff(&also.v).map(|d| (m, d))) {
                Some((m, d)) if d.your_sites_changed == 0 => format!(
                    "Moving yours to {} drops a copy; none of {} change.",
                    semver::short(&also.v),
                    m.your_uses.map_or_else(|| "your uses".to_owned(), |n| format!("your {n} uses"))
                ),
                Some((_, d)) => format!(
                    "Moving yours to {} drops a copy; {} change.",
                    semver::short(&also.v),
                    plural(d.your_sites_changed, "of your uses", "of your uses")
                ),
                None => format!("Moving yours to {} would drop a copy.", semver::short(&also.v)),
            }
        };
        (say, fact)
    }
}

/// What the number's card says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberCard {
    /// Your pin, and the newest release when you are behind it.
    pub head: (String, Option<String>),
    /// The semver reading.
    pub reading: String,
    /// What an upgrade does to your code, when measured.
    pub yours: Option<String>,
    /// The other copies your tree holds.
    pub dup: Option<String>,
}

/// The version mark (see the module docs).
#[derive(IntoElement)]
pub struct VersionMark {
    id: ElementId,
    facts: Rc<VersionFacts>,
    measure: Measure,
    style: CombStyle,
    number_look: bool,
    also_look: Option<String>,
    on_select: Option<Rc<dyn Fn(Option<&str>, &mut Window, &mut App)>>,
    viewing: Option<String>,
    scrub: Vec<(u64, String)>,
}

/// The version mark for `facts` (the Rider), as wide as `measure`.
#[must_use]
pub fn version_mark(id: impl Into<ElementId>, facts: VersionFacts, measure: &Measure) -> VersionMark {
    VersionMark {
        id: id.into(),
        facts: Rc::new(facts),
        measure: *measure,
        style: CombStyle::Rider,
        number_look: false,
        also_look: None,
        on_select: None,
        viewing: None,
        scrub: Vec::new(),
    }
}

impl VersionMark {
    /// The Baseline, for list rows.
    #[must_use]
    pub const fn row(mut self) -> Self {
        self.style = CombStyle::Baseline;
        self
    }

    /// Opens the number's card on the first paint (state sheets).
    #[must_use]
    pub const fn number_look(mut self) -> Self {
        self.number_look = true;
        self
    }

    /// Opens the card of the also tooth at `v` (state sheets).
    #[must_use]
    pub fn also_look(mut self, v: impl Into<String>) -> Self {
        self.also_look = Some(v.into());
        self
    }

    /// The release being read (a page scoped to another release); `None`
    /// reads the pin.
    #[must_use]
    pub fn viewing(mut self, v: Option<String>) -> Self {
        self.viewing = v;
        self
    }

    /// Called when the reader scrubs to a release (`None`: back to the pin).
    /// Without it, the mark keeps the scrub to itself.
    #[must_use]
    pub fn on_select(mut self, select: impl Fn(Option<&str>, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(select));
        self
    }

    /// Scrubs to `v` `after` ms into the scene, as a click on its tick
    /// would (films).
    #[must_use]
    pub fn scrub_at(mut self, after: u64, v: impl Into<String>) -> Self {
        self.scrub.push((after, v.into()));
        self
    }

    /// The style this width gets on its own: a band below 240 px. (What is
    /// drawn holds its style through the edge's band: see `render`.)
    #[must_use]
    pub fn style_for(&self) -> CombStyle {
        self.style_in(COMB.at(self.measure.fluid_room()))
    }

    fn style_in(&self, comb: Comb) -> CombStyle {
        match comb {
            Comb::Band => CombStyle::Band,
            Comb::Asked => self.style,
        }
    }
}

fn step_of(kind: semver::Tick) -> Step {
    match kind {
        semver::Tick::Breaking => Step::Major,
        semver::Tick::Minor => Step::Minor,
        semver::Tick::Patch | semver::Tick::Pre => Step::Patch,
    }
}

impl RenderOnce for VersionMark {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let measure = self.measure;
        let s = measure.scale();
        let facts = self.facts.clone();
        if facts.local.is_some() || facts.releases.is_empty() {
            return unpublished(&self.id, &facts, &measure, self.number_look, window, cx);
        }
        let sorted = facts.sorted();
        let versions: Vec<&str> = sorted.iter().map(|r| r.v.as_str()).collect();
        let kinds = semver::kinds(&versions);
        let releases: Vec<Release> = sorted
            .iter()
            .zip(&kinds)
            .map(|(r, kind)| Release {
                id: ReleaseId(r.v.clone().into()),
                version: semver::short(&r.v).to_owned().into(),
                step: step_of(*kind),
                age: r
                    .at
                    .as_deref()
                    .and_then(|at| semver::ago(at, &facts.now))
                    .map_or_else(|| "age unknown".into(), |a| format!("{a} ago").into()),
            })
            .collect();
        let index = |v: &str| sorted.iter().position(|r| r.v == v || semver::short(&r.v) == semver::short(v));
        let pinned = facts.pin.as_deref().and_then(index);
        // The mark keeps its own scrub unless the page drives it.
        let own = window.use_keyed_state(ElementId::NamedChild(Arc::new(self.id.clone()), "scrub".into()), cx, |_, _| {
            Rc::new(Cell::new(None::<usize>))
        });
        let own = own.read(cx).clone();
        if !self.scrub.is_empty() && self.on_select.is_none() {
            let once = window.use_keyed_state(ElementId::NamedChild(Arc::new(self.id.clone()), "scrub-at".into()), cx, |_, _| {
                Rc::new(Cell::new(false))
            });
            let once = once.read(cx).clone();
            if !once.replace(true) {
                let pin = facts.pin.as_deref().and_then(index);
                for (after, v) in self.scrub.clone() {
                    let target = index(&v).filter(|i| Some(*i) != pin);
                    let own = own.clone();
                    window
                        .spawn(cx, async move |cx| {
                            cx.background_executor().timer(std::time::Duration::from_millis(after)).await;
                            let _ = cx.update(|window, _| {
                                own.set(target);
                                window.refresh();
                            });
                        })
                        .detach();
                }
            }
        }
        let selected = match (&self.on_select, &self.viewing) {
            (Some(_), Some(v)) => index(v),
            (Some(_), None) => None,
            (None, _) => own.get(),
        };
        let latest = semver::reading(&facts.releases, facts.pin.as_deref(), &facts.now).latest.as_deref().and_then(index);
        let number = facts.number_lines();
        let number_card: Content = Rc::new(move |measure: &Measure, _window: &mut Window, cx: &mut App| {
            number_card(&number, measure, cx)
        });
        let mut teeth = Vec::new();
        let mut also_look = None;
        for also in facts.also.iter().filter(|a| Some(&a.v) != facts.pin.as_ref()) {
            let Some(i) = index(&also.v) else { continue };
            let (say, fact) = facts.also_lines(also);
            let v = semver::short(&also.v).to_owned();
            if self.also_look.as_deref().is_some_and(|look| semver::short(look) == v) {
                also_look = Some(i);
            }
            teeth.push(AlsoTooth {
                index: i,
                card: Rc::new(move |measure: &Measure, _window: &mut Window, cx: &mut App| also_card(&v, &say, &fact, measure, cx)),
            });
        }
        let yanked: Vec<usize> = sorted.iter().enumerate().filter(|(_, r)| r.yanked).map(|(i, _)| i).collect();
        let held = Modes::keyed(ElementId::NamedChild(Arc::new(self.id.clone()), "style".into()), window, cx);
        let style = self.style_in(held.settle(&COMB, measure.fluid_room()).mode);
        let ids: Vec<String> = sorted.iter().map(|r| r.v.clone()).collect();
        let external = self.on_select.clone();
        let own_select = own.clone();
        let pin_index = pinned;
        let card = number_card.clone();
        let mut comb = version_comb(ElementId::NamedChild(Arc::new(self.id.clone()), "comb".into()), releases, &measure)
            .style(style)
            .yanked(yanked)
            .also(teeth)
            .number_card(move |m, w, cx| card(m, w, cx))
            .on_select(move |selected: &VersionSelected, window, cx| {
                let at = ids.iter().position(|v| *v == selected.0.0.as_ref());
                let home = at == pin_index;
                match &external {
                    Some(select) => select(if home { None } else { at.map(|i| ids[i].as_str()) }, window, cx),
                    None => {
                        own_select.set(if home { None } else { at });
                        window.refresh();
                    }
                }
            });
        if let Some(pin) = pinned {
            comb = comb.pinned(pin);
        }
        if let Some(selected) = selected {
            comb = comb.selected(selected);
        }
        if let Some(latest) = latest {
            comb = comb.latest(latest);
        }
        if self.number_look {
            comb = comb.number_look();
        }
        if let Some(i) = also_look {
            comb = comb.also_look(i);
        }
        let _ = (palette, s);
        comb.into_any_element()
    }
}

fn number_card(lines: &NumberCard, measure: &Measure, cx: &mut App) -> AnyElement {
    let palette = cx.facet().palette();
    let mut head = div()
        .flex()
        .items_baseline()
        .gap(k(measure, 8.0))
        .child(text("mk-ver-pin", lines.head.0.clone(), card::TITLE_MONO, measure, palette.ink0));
    if let Some(latest) = &lines.head.1 {
        head = head
            .child(text("mk-ver-to", "→", card::FACT, measure, palette.ink4))
            .child(text("mk-ver-latest", latest.clone(), card::TITLE_MONO, measure, palette.ink2));
    }
    let mut body = card::body(340.0, measure)
        .child(head)
        .child(div().mt(k(measure, 5.0)).child(text("mk-ver-read", lines.reading.clone(), card::READ, measure, palette.ink1)));
    if let Some(yours) = &lines.yours {
        body = body.child(div().mt(k(measure, 8.0)).child(text("mk-ver-yours", yours.clone(), card::SAY, measure, palette.ink1)));
    }
    if let Some(dup) = &lines.dup {
        body = body.child(div().mt(k(measure, 8.0)).child(text("mk-ver-dup", dup.clone(), card::FACT, measure, palette.ink2)));
    }
    body.into_any_element()
}

fn also_card(v: &str, say: &str, fact: &str, measure: &Measure, cx: &mut App) -> AnyElement {
    let palette = cx.facet().palette();
    card::body(340.0, measure)
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(k(measure, 8.0))
                .child(text("mk-also-v", v.to_owned(), card::TITLE_MONO, measure, palette.ink0))
                .child(text("mk-also-place", "also in your tree", card::SAY, measure, palette.ink3)),
        )
        .child(div().mt(k(measure, 8.0)).child(text("mk-also-say", say.to_owned(), card::SAY, measure, palette.ink1)))
        .child(div().mt(k(measure, 8.0)).child(text("mk-also-fact", fact.to_owned(), card::FACT, measure, palette.ink2)))
        .into_any_element()
}

/// A package with no published history: one tooth, a dashed line.
fn unpublished(id: &ElementId, facts: &VersionFacts, measure: &Measure, sheet: bool, window: &mut Window, cx: &mut App) -> AnyElement {
    let palette = cx.facet().palette();
    let s = measure.scale();
    let v = facts.pin.clone().unwrap_or_default();
    let key = ElementId::NamedChild(Arc::new(id.clone()), "card".into());
    let live = card::live(id, &key, window, cx);
    let (place, say) = if facts.local.is_some() {
        (
            "never published",
            "Its version comes from the workspace, and publishing is off: nothing outside this repository can depend on it.".to_owned(),
        )
    } else {
        ("no releases known", format!("No release history is known for {}.", facts.name))
    };
    let content: Content = {
        let v = v.clone();
        Rc::new(move |measure: &Measure, _window: &mut Window, cx: &mut App| {
            let palette = cx.facet().palette();
            card::body(340.0, measure)
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(k(measure, 8.0))
                        .child(text("mk-ver-pin", v.clone(), card::TITLE_MONO, measure, palette.ink0))
                        .child(text("mk-ver-place", place, card::SAY, measure, palette.ink3)),
                )
                .child(div().mt(k(measure, 8.0)).child(text("mk-ver-say", say.clone(), card::SAY, measure, palette.ink1)))
                .into_any_element()
        })
    };
    let number = div()
        .relative()
        .h(px(17.0 * s))
        .flex()
        .items_end()
        .pl(px(8.0 * s))
        .child(div().absolute().left_0().bottom_0().w(px(2.0 * s)).h(px(17.0 * s)).bg(palette.mint.base.hsla()))
        .child(text(ElementId::NamedChild(Arc::new(id.clone()), "number".into()), v, card::LINE, measure, palette.ink0));
    let mut dashes = Vec::new();
    let mut x = px(0.0);
    while x < px(120.0 * s) {
        dashes.push(
            div()
                .absolute()
                .left(x)
                .bottom(px(3.5 * s))
                .w(px(3.0 * s))
                .h(px(1.0))
                .bg(palette.ink4.hsla()),
        );
        x += px(7.0 * s);
    }
    let dashed = div().relative().w(px(120.0 * s)).h(px(20.0 * s)).children(dashes);
    let mark = div().flex().items_end().gap(px(8.0 * s)).h(px(28.0 * s)).pb(px(3.0 * s)).child(number).child(dashed);
    card::door(id, &key, &live, Some(content), sheet.then_some(0), None, mark)
}

/// The number card's content on its own (boards show it in place).
#[cfg(feature = "gallery")]
pub(crate) fn board_number(facts: &VersionFacts) -> Content {
    let lines = facts.number_lines();
    Rc::new(move |measure: &Measure, _window: &mut Window, cx: &mut App| number_card(&lines, measure, cx))
}

/// An also tooth's card content on its own (boards show it in place).
#[cfg(feature = "gallery")]
pub(crate) fn board_also(facts: &VersionFacts, v: &str) -> Option<Content> {
    let also = facts.also.iter().find(|a| semver::short(&a.v) == semver::short(v))?.clone();
    let (say, fact) = facts.also_lines(&also);
    let v = semver::short(&also.v).to_owned();
    Some(Rc::new(move |measure: &Measure, _window: &mut Window, cx: &mut App| also_card(&v, &say, &fact, measure, cx)))
}
