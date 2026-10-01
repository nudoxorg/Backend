//! The package folio: the hero's crest, the release ticker, and the
//! territory of shingles that opens into cards. This is the component that
//! remembers which module is open; everything it draws comes from `facet`.

use super::data::{Diffs, ModuleData, Past, Side, Structure};
use super::fluid::{self, Tracks};
use super::target::{Mark, PageTarget};
use crate::model::pages::{PackageRef, PageKey};
use crate::model::source_facts::Reading;
use crate::navigation::Intent;
use crate::shell::focus::{Act, Recall, Target, Targets};
use crate::shell::kit::{package_route, symbol_route};
use crate::shell::region::Links;
use facet::ActiveFacet as _;
use facet::fluid::Modes;
use facet::folio::berg::{BergFacts, berg as berg_view, weight};
use facet::folio::cards::CardFacts;
use facet::folio::crest::{self, Advisories, REST};
use facet::folio::features::{FeatureFacts, feature_preview};
use facet::folio::flight::{Marks, Stone, flight, progress};
use facet::folio::heads::{Finding, heads, open_sheet};
use facet::folio::module::module as module_view;
use facet::folio::rail::rail;
use facet::folio::shingles::{ModuleFacts, ShingleFacts, Spot, shingles};
use facet::folio::state::{Extent, Fold, Pose, Time, Use};
use facet::folio::text::{key, one};
use facet::folio::ticker::{TickerFacts, ticker};
use facet::marks::badges::{Glyph, Item, glyph};
use facet::marks::license::LicenseFacts;
use facet::motion::flow::FlowItem;
use facet::motion::{Carry, Flow, Presence, request_frame};
use facet::tokens::fluid::Crest;
use facet::tokens::{TypeRole, ty};
use facet::{Measure, Space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, ElementId, Entity, InteractiveElement, IntoElement, ParentElement,
    Pixels, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
};
use std::rc::Rc;

/// What the folio remembers between frames.
#[derive(Clone, Debug, Default)]
struct Nav {
    /// The module open, by name.
    open: Option<SharedString>,
    /// The release whose contents the open module belongs to.
    reading_at: Option<SharedString>,
    /// The name a click on a shingle asked for.
    lit: Option<SharedString>,
    /// Whether the full berg is showing.
    berg: Fold,
    /// Whether the licence stamp is held unfolded (Enter on it).
    licence: Pose,
    /// The shingles of the module just opened, on their way to its cards.
    flight: Option<Flying>,
    /// A different module chosen while the current carry is still returning.
    pending: Option<Pending>,
    /// Where the open module's cards report their marks.
    marks: Marks,
}

/// A module's shingles carried to its cards, and when the carry began.
#[derive(Clone, Debug)]
struct Flying {
    module: SharedString,
    stones: Rc<[Stone]>,
    carry: Carry,
}

/// A route requested while a different module's stones are returning home.
#[derive(Clone, Debug)]
struct Pending {
    module: SharedString,
    lit: Option<SharedString>,
    stones: Option<Rc<[Stone]>>,
}

const CARRY_REST: f32 = 0.005;

fn close_module(nav: &mut Nav, now: std::time::Instant) {
    nav.open = None;
    nav.lit = None;
    nav.pending = None;
    if let Some(flying) = nav.flight.as_mut()
        && flying.carry.target() != 0.0
    {
        flying.carry.retarget(0.0, now);
    }
}

fn read_release(nav: &mut Nav, reading_at: Option<SharedString>, now: std::time::Instant) {
    if nav.reading_at != reading_at {
        close_module(nav, now);
        nav.reading_at = reading_at;
    }
}

fn leave_for_symbol(nav: &mut Nav, now: std::time::Instant) {
    let Some(module) = nav.open.clone() else { return };
    let Some(flying) = nav.flight.as_ref() else { return };
    if flying.module != module {
        return;
    }
    queue_module(
        nav,
        Pending { module, lit: nav.lit.clone(), stones: None },
        now,
    );
}

fn queue_module(nav: &mut Nav, pending: Pending, now: std::time::Instant) {
    nav.open = None;
    nav.lit = None;
    nav.pending = Some(pending);
    if let Some(flying) = nav.flight.as_mut()
        && flying.carry.target() != 0.0
    {
        flying.carry.retarget(0.0, now);
    }
}

fn begin_carry(nav: &mut Nav, module: SharedString, stones: Vec<Stone>, now: std::time::Instant) {
    if let Some(flying) = nav.flight.as_mut()
        && flying.module == module
    {
        nav.pending = None;
        flying.carry.retarget(1.0, now);
        return;
    }
    if nav
        .flight
        .as_ref()
        .is_some_and(|flying| !flying.carry.settled(now, CARRY_REST))
    {
        queue_module(nav, Pending { module, lit: None, stones: Some(stones.into()) }, now);
        return;
    }
    nav.pending = None;
    nav.flight = Some(Flying {
        module,
        stones: stones.into(),
        carry: Carry::new(0.0, 1.0, now),
    });
}

fn request_module(
    nav: &mut Nav,
    module: SharedString,
    lit: Option<SharedString>,
    now: std::time::Instant,
) {
    if let Some(flying) = nav.flight.as_mut()
        && flying.module == module
    {
        nav.pending = None;
        flying.carry.retarget(1.0, now);
        nav.open = Some(module);
        nav.lit = lit;
        return;
    }
    if nav
        .flight
        .as_ref()
        .is_some_and(|flying| !flying.carry.settled(now, CARRY_REST))
    {
        let stones = nav
            .pending
            .as_ref()
            .filter(|pending| pending.module == module)
            .and_then(|pending| pending.stones.clone());
        queue_module(nav, Pending { module, lit, stones }, now);
        return;
    }
    nav.flight = None;
    nav.pending = None;
    nav.open = Some(module);
    nav.lit = lit;
}

fn finish_carry(nav: &mut Nav, now: std::time::Instant, reduced: bool) {
    let returning = nav.flight.as_ref().is_some_and(|flying| flying.carry.target() == 0.0);
    let landed = nav
        .flight
        .as_ref()
        .is_some_and(|flying| flying.carry.settled(now, CARRY_REST));
    if reduced || (returning && landed) {
        nav.flight = None;
        if let Some(pending) = nav.pending.take() {
            nav.open = Some(pending.module.clone());
            nav.lit = pending.lit;
            if !reduced
                && let Some(stones) = pending.stones
            {
                nav.flight = Some(Flying {
                    module: pending.module,
                    stones,
                    carry: Carry::new(0.0, 1.0, now),
                });
            }
        }
    }
}

/// Everything the folio draws.
pub(super) struct Facts {
    /// The package as its page names it.
    pub name: SharedString,
    /// The release being read (not the pin), when the route says so.
    pub at: Option<SharedString>,
    /// The release you pin.
    pub pin: Option<SharedString>,
    /// Its modules.
    pub modules: Vec<ModuleData>,
    /// What those modules are: recorded ones, a flat root, or names gathered
    /// for want of a module.
    pub structure: Structure,
    /// What the source on disk said (or is still saying).
    pub source: Reading,
    /// What the package does, from its source.
    pub heads: Option<Rc<Vec<Finding>>>,
    /// What it weighs, from its source and everything beneath it.
    pub berg: Option<Rc<BergFacts>>,
    /// Its features, from its manifest.
    pub features: Option<Rc<FeatureFacts>>,
    /// The licence stamp's facts.
    pub licence: Rc<LicenseFacts>,
    /// What the advisory feeds say.
    pub advisories: Advisories,
    /// The releases.
    pub ticker: Option<Rc<TickerFacts>>,
    /// Why the exact local release history is pending or incomplete.
    pub ticker_note: Option<SharedString>,
    /// What the release being read did to the names.
    pub past: Option<Past>,
    /// How many names have a first sentence, of how many.
    pub documented: Option<(usize, usize)>,
    /// Why there are no names to draw, when the index has not served them.
    pub outline_gap: Option<SharedString>,
}

/// The folio (see the module docs).
#[derive(IntoElement)]
pub(super) struct Folio {
    pub id: ElementId,
    pub facts: Rc<Facts>,
    pub measure: Measure,
    pub hero: AnyElement,
    pub links: Links,
    pub targets: Targets,
    /// What a click that leaves the page writes, so Back returns to it.
    pub recall: Recall,
    /// The module a click left the page from, open again on coming back.
    pub reopen: Option<SharedString>,
    pub active: bool,
    pub package: PackageRef,
}

const HEADER: TypeRole = TypeRole {
    size: 13.0,
    line: 18.0,
    ..ty::SMALL
};
const HEADER_NUMBER: TypeRole = TypeRole {
    weight: 600.0,
    size: 13.0,
    line: 18.0,
    ..ty::MONO_SMALL
};
const BANNER_WORD: TypeRole = TypeRole {
    size: 12.5,
    line: 18.0,
    ..ty::SMALL
};
const BANNER_VERSION: TypeRole = TypeRole {
    weight: 600.0,
    size: 12.5,
    line: 18.0,
    ..ty::MONO_SMALL
};

impl Folio {
    fn state(&self, window: &mut Window, cx: &mut App) -> Entity<Nav> {
        let reopen = self.reopen.clone();
        let reading_at = self.facts.at.clone();
        window.use_keyed_state(self.id.clone(), cx, move |_, _| Nav {
            open: reopen,
            reading_at,
            ..Nav::default()
        })
    }

    fn map_modules(&self) -> Rc<[ModuleFacts]> {
        let past = self.facts.past.as_ref();
        self.facts
            .modules
            .iter()
            .map(|module| {
                let mut facts = ModuleFacts::new(
                    module.name.clone(),
                    module
                        .items
                        .iter()
                        .map(|item| ShingleFacts {
                            name: item.name.clone(),
                            family: item.family,
                            yours: Use::Elsewhere,
                            state: past.and_then(|past| past.states.get(&item.name).copied()),
                        })
                        .collect(),
                );
                facts.doc = module.doc.clone();
                facts.extent = module.extent();
                facts
            })
            .collect::<Vec<_>>()
            .into()
    }

    /// The crest: licence, heads-up, weight, advisories, laid out on
    /// `tracks` (see `fluid`), each cell springing to its place when the
    /// arrangement changes.
    fn crest(
        &self,
        tracks: &Tracks,
        flow: &Flow,
        measure: &Measure,
        nav: &Entity<Nav>,
        berg_open: Fold,
        licence: Pose,
    ) -> AnyElement {
        let (stamp_w, cell_w, spread) = (tracks.stamp, tracks.cell, tracks.spread);
        let source_words = |what: &str| -> SharedString {
            match &self.facts.source {
                Reading::Reading => format!("Reading its source on disk for {what}.").into(),
                Reading::Waiting => {
                    format!("Waiting for an available source-read slot for {what}.").into()
                }
                Reading::Absent(why) => format!("{why} Nothing is read for {what}.").into(),
                Reading::Ready(_) => SharedString::default(),
            }
        };
        let heads_cell = match &self.facts.heads {
            Some(findings) => heads(
                key(&self.id, "heads"),
                self.facts.name.clone(),
                findings.clone(),
                cell_w,
                spread,
                REST,
                measure,
            )
            .into_any_element(),
            None => crest::unread(
                key(&self.id, "heads"),
                "Heads-up",
                source_words("build scripts, network, files and unsafe"),
                cell_w,
                measure,
            )
            .into_any_element(),
        };
        let weight_cell = match &self.facts.berg {
            Some(facts) => {
                let state = nav.clone();
                weight(
                    key(&self.id, "weight"),
                    facts.clone(),
                    cell_w,
                    REST,
                    berg_open,
                    measure,
                )
                .on_toggle(move |_window, cx| {
                    state.update(cx, |nav, cx| {
                        nav.berg = if nav.berg == Fold::Open {
                            Fold::Folded
                        } else {
                            Fold::Open
                        };
                        cx.notify();
                    });
                })
                .into_any_element()
            }
            None => crest::unread(
                key(&self.id, "weight"),
                "Weight",
                source_words("lines of code"),
                cell_w,
                measure,
            )
            .into_any_element(),
        };
        let cell = |name: &str, element: AnyElement| {
            flow.item(key(&self.id, format!("flow-{name}")), element)
        };
        // Each cell the reader can act on is a door: `j`/`k` reach it, Enter
        // is what a click on it does.
        let licence_act: Act = {
            let state = nav.clone();
            Rc::new(move |_, cx| {
                state.update(cx, |nav, cx| {
                    nav.licence = if nav.licence == Pose::Held {
                        Pose::Live
                    } else {
                        Pose::Held
                    };
                    cx.notify();
                });
            })
        };
        let heads_act: Option<Act> = self.facts.heads.clone().map(|findings| {
            let package = self.facts.name.clone();
            let act: Act =
                Rc::new(move |window, cx| open_sheet(&package, findings.clone(), window, cx));
            act
        });
        let weight_act: Option<Act> = self.facts.berg.as_ref().map(|_| {
            let state = nav.clone();
            let act: Act = Rc::new(move |_, cx| {
                state.update(cx, |nav, cx| {
                    nav.berg = if nav.berg == Fold::Open {
                        Fold::Folded
                    } else {
                        Fold::Open
                    };
                    cx.notify();
                });
            });
            act
        });
        let stamp = crest::stamp(
            key(&self.id, "licence"),
            self.facts.licence.clone(),
            stamp_w,
            measure,
        )
        .pose(licence)
        .into_any_element();
        let cells = [
            cell(
                "licence",
                door(
                    &self.targets,
                    self.active,
                    &PageTarget::Licence,
                    "Licence",
                    licence_act,
                    stamp,
                ),
            ),
            cell(
                "heads",
                match heads_act {
                    Some(act) => door(
                        &self.targets,
                        self.active,
                        &PageTarget::Heads,
                        "Heads-up",
                        act,
                        heads_cell,
                    ),
                    None => heads_cell,
                },
            ),
            cell(
                "weight",
                match weight_act {
                    Some(act) => door(
                        &self.targets,
                        self.active,
                        &PageTarget::Weight,
                        "Weight",
                        act,
                        weight_cell,
                    ),
                    None => weight_cell,
                },
            ),
            cell(
                "advisories",
                crest::advisories(
                    key(&self.id, "advisories"),
                    self.facts.advisories.clone(),
                    cell_w,
                    measure,
                )
                .into_any_element(),
            ),
        ];
        // The rows are the arrangement's, named, never left to a wrap: the
        // cells sum to their row's width, and a wrap would drop the last one
        // onto a row of its own at the first pixel of error.
        let row = |cells: Vec<FlowItem>| div().flex().items_start().gap(tracks.gap).children(cells);
        let mut cells = cells.into_iter();
        match tracks.crest {
            Crest::Four => row(cells.collect()).into_any_element(),
            Crest::Two => {
                let (first, second): (Vec<_>, Vec<_>) =
                    cells.enumerate().partition(|(index, _)| *index < 2);
                div()
                    .flex()
                    .flex_col()
                    .gap(tracks.gap)
                    .child(row(first.into_iter().map(|(_, cell)| cell).collect()))
                    .child(row(second.into_iter().map(|(_, cell)| cell).collect()))
                    .into_any_element()
            }
            Crest::One => div()
                .flex()
                .flex_col()
                .gap(tracks.gap)
                .children(cells.by_ref().map(|cell| row(vec![cell])))
                .into_any_element(),
        }
    }

    /// The banner that says the page is in the past.
    fn banner(
        &self,
        past: &Past,
        measure: &Measure,
        palette: &'static facet::Palette,
    ) -> AnyElement {
        let words = match past.diffs {
            Diffs::Known if past.side == Side::Before => format!(
                "{} of today's names did not exist yet · {} looked different",
                past.absent, past.changed
            ),
            Diffs::Known => format!(
                "{} new names · {} changed · {} gone",
                past.added, past.changed, past.gone
            ),
            Diffs::SourceUnavailable => "This release's source is not on this machine.".to_owned(),
            Diffs::SourceAmbiguous => past.note.clone().unwrap_or_else(|| {
                "This release has conflicting registry sources. Resolve the Cargo registry authority first.".to_owned()
            }),
            Diffs::SourceUnverified => past.note.clone().unwrap_or_else(|| {
                "This local archive has no trusted registry checksum.".to_owned()
            }),
            Diffs::DiffUnavailable => {
                "The source is local, but its exact comparison is unavailable.".to_owned()
            }
            Diffs::Unknown => past
                .note
                .clone()
                .unwrap_or_else(|| "Checking this exact release comparison…".to_owned()),
        };
        let relation = if past.side == Side::Before {
            "before"
        } else {
            "after"
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(measure.space(Space::Gutter))
            .gap_y(measure.space(Space::Tight))
            .px(measure.space(Space::Roomy))
            .py(measure.space(Space::Snug))
            .bg(palette.plate.hsla())
            .border_l_2()
            .border_color(palette.amber.base.hsla())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(measure.space(Space::Snug))
                    .child(glyph(
                        Glyph::Marker,
                        12.0 * measure.scale(),
                        palette.amber.base,
                    ))
                    .child(one(
                        key(&self.id, "reading"),
                        "Reading",
                        BANNER_WORD,
                        palette.ink1,
                        measure,
                    ))
                    .child(one(
                        key(&self.id, "at"),
                        past.at.clone(),
                        BANNER_VERSION,
                        palette.amber.base,
                        measure,
                    ))
                    .child(one(
                        key(&self.id, "relation"),
                        format!("{relation} your pin"),
                        BANNER_WORD,
                        palette.ink1,
                        measure,
                    )),
            )
            .child(one(
                key(&self.id, "words"),
                words,
                BANNER_WORD,
                palette.ink1,
                measure,
            ))
            .child(div().flex_1())
            // The way out of the past, always in reach.
            .child({
                let links = self.links.clone();
                facet::controls::button(key(&self.id, "back"), "back to your pin", measure)
                    .ghost()
                    .size(facet::Control::Small)
                    .key("esc")
                    .on_click(move |_window, cx| links.dispatch(Intent::SetRelease(None), cx))
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Carry, Flying, Nav, Pending, Stone, begin_carry, close_module, finish_carry,
        leave_for_symbol, read_release, request_module,
    };
    use gpui::Bounds;
    use std::time::{Duration, Instant};

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    fn stone() -> Stone {
        Stone { from: Bounds::default(), ink: gpui::Hsla::default() }
    }

    #[test]
    fn escape_and_reopen_retarget_the_retained_carry_without_a_pose_or_velocity_jump() {
        let start = Instant::now();
        let stone = stone();
        let mut nav = Nav {
            open: Some("sync".into()),
            flight: Some(Flying {
                module: "sync".into(),
                stones: vec![stone].into(),
                carry: Carry::new(0.0, 1.0, start),
            }),
            ..Nav::default()
        };

        let close_at = at(start, 120);
        let opening_pose = nav.flight.as_ref().unwrap().carry.sample(close_at);
        close_module(&mut nav, close_at);
        let closing_pose = nav.flight.as_ref().unwrap().carry.sample(close_at);
        assert!((closing_pose.0 - opening_pose.0).abs() < 1e-5);
        assert!((closing_pose.1 - opening_pose.1).abs() < 1e-4);
        assert_eq!(nav.flight.as_ref().unwrap().carry.target(), 0.0);
        assert_eq!(nav.flight.as_ref().unwrap().stones[0], stone);

        let reopen_at = at(start, 180);
        let before_reopen = nav.flight.as_ref().unwrap().carry.sample(reopen_at);
        request_module(&mut nav, "sync".into(), Some("Mutex".into()), reopen_at);
        let after_reopen = nav.flight.as_ref().unwrap().carry.sample(reopen_at);
        assert!((after_reopen.0 - before_reopen.0).abs() < 1e-5);
        assert!((after_reopen.1 - before_reopen.1).abs() < 1e-4);
        assert_eq!(nav.flight.as_ref().unwrap().carry.target(), 1.0);
        assert_eq!(nav.open.as_deref(), Some("sync"));
        assert_eq!(nav.lit.as_deref(), Some("Mutex"));
        assert!(nav.flight.as_ref().unwrap().carry.value(at(start, 1200)) > 0.99);
    }

    #[test]
    fn changing_modules_finishes_the_old_return_before_starting_the_queued_carry() {
        let start = Instant::now();
        let destination = stone();
        let mut nav = Nav {
            open: Some("sync".into()),
            flight: Some(Flying {
                module: "sync".into(),
                stones: vec![stone()].into(),
                carry: Carry::new(0.0, 1.0, start),
            }),
            ..Nav::default()
        };

        let changed_at = at(start, 100);
        begin_carry(&mut nav, "io".into(), vec![destination], changed_at);
        request_module(&mut nav, "io".into(), Some("Read".into()), changed_at);
        assert_eq!(nav.open, None);
        assert_eq!(nav.pending.as_ref().map(|pending| pending.module.as_ref()), Some("io"));
        assert_eq!(nav.flight.as_ref().unwrap().carry.target(), 0.0);

        let landed_at = at(start, 1200);
        finish_carry(&mut nav, landed_at, false);
        assert_eq!(nav.open.as_deref(), Some("io"));
        assert_eq!(nav.lit.as_deref(), Some("Read"));
        assert!(nav.pending.is_none());
        assert_eq!(nav.flight.as_ref().unwrap().module.as_ref(), "io");
        assert_eq!(nav.flight.as_ref().unwrap().stones[0], destination);
        assert_eq!(nav.flight.as_ref().unwrap().carry.target(), 1.0);
        assert_eq!(nav.flight.as_ref().unwrap().carry.value(landed_at), 0.0);
    }

    #[test]
    fn reduced_motion_lands_a_queued_route_at_its_endpoint() {
        let start = Instant::now();
        let mut nav = Nav {
            flight: Some(Flying {
                module: "sync".into(),
                stones: vec![stone()].into(),
                carry: Carry::new(0.0, 1.0, start),
            }),
            pending: Some(Pending { module: "io".into(), lit: None, stones: None }),
            ..Nav::default()
        };

        finish_carry(&mut nav, at(start, 80), true);
        assert_eq!(nav.open.as_deref(), Some("io"));
        assert!(nav.flight.is_none());
        assert!(nav.pending.is_none());
    }

    #[test]
    fn leaving_for_a_symbol_returns_the_module_carry_and_restores_it_on_back() {
        let start = Instant::now();
        let mut nav = Nav {
            open: Some("sync".into()),
            lit: Some("Mutex".into()),
            flight: Some(Flying {
                module: "sync".into(),
                stones: vec![stone()].into(),
                carry: Carry::new(0.0, 1.0, start),
            }),
            ..Nav::default()
        };

        let left_at = at(start, 120);
        let pose = nav.flight.as_ref().unwrap().carry.sample(left_at);
        leave_for_symbol(&mut nav, left_at);
        assert_eq!(nav.open, None);
        assert_eq!(nav.pending.as_ref().and_then(|pending| pending.lit.as_deref()), Some("Mutex"));
        assert_eq!(nav.flight.as_ref().unwrap().carry.sample(left_at), pose);

        finish_carry(&mut nav, at(start, 1200), false);
        assert_eq!(nav.open.as_deref(), Some("sync"));
        assert_eq!(nav.lit.as_deref(), Some("Mutex"));
        assert!(nav.flight.is_none());
    }

    #[test]
    fn changing_release_retargets_the_retained_carry_back_to_the_new_map() {
        let start = Instant::now();
        let mut nav = Nav {
            open: Some("sync".into()),
            reading_at: Some("1.0".into()),
            flight: Some(Flying {
                module: "sync".into(),
                stones: vec![stone()].into(),
                carry: Carry::new(0.0, 1.0, start),
            }),
            ..Nav::default()
        };

        let changed_at = at(start, 120);
        let pose = nav.flight.as_ref().unwrap().carry.sample(changed_at);
        read_release(&mut nav, Some("2.0".into()), changed_at);
        assert_eq!(nav.open, None);
        assert_eq!(nav.reading_at.as_deref(), Some("2.0"));
        assert_eq!(nav.flight.as_ref().unwrap().carry.target(), 0.0);
        assert_eq!(nav.flight.as_ref().unwrap().carry.sample(changed_at).0, pose.0);

        finish_carry(&mut nav, at(start, 1200), false);
        assert!(nav.flight.is_none());
        assert!(nav.pending.is_none());
    }
}

/// A keyboard door for `target` around `element`: the shell's `j`/`k` walk to
/// it and Enter runs `act` (only on the page the keyboard is on).
pub(super) fn door(
    targets: &Targets,
    active: bool,
    target: &PageTarget,
    label: impl Into<SharedString>,
    act: Act,
    element: impl IntoElement,
) -> AnyElement {
    let id = target.id();
    if active {
        targets.push(Target {
            id: id.clone(),
            label: label.into(),
            act,
            peek: None,
            source: None,
        });
    }
    targets.track(id, element).into_any_element()
}

/// One door laid over an element: which target it is, what it reads as, where
/// it sits (relative to the element's corner) and what Enter does.
struct Door {
    target: PageTarget,
    label: SharedString,
    at: Bounds<Pixels>,
    act: Act,
}

/// `element` with invisible doors laid over it: the element paints, the doors
/// are what the keyboard stands on and what the focus bevel travels between.
fn doors(
    targets: &Targets,
    active: bool,
    element: impl IntoElement,
    over: Vec<Door>,
) -> AnyElement {
    let mut holder = div().relative().child(element);
    for Door {
        target,
        label,
        at,
        act,
    } in over
    {
        let id = target.id();
        if active {
            targets.push(Target {
                id: id.clone(),
                label,
                act,
                peek: None,
                source: None,
            });
        }
        let frame = div()
            .absolute()
            .left(at.origin.x)
            .top(at.origin.y)
            .w(at.size.width)
            .h(at.size.height);
        holder = holder.child(targets.track(id, frame));
    }
    holder.into_any_element()
}

impl RenderOnce for Folio {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let measure = self.measure;
        let facts = self.facts.clone();
        let nav = self.state(window, cx);
        let modules = self.map_modules();
        let now = facet::motion::now(cx);
        let reduce_motion = facet::motion::reduced(cx);
        nav.update(cx, |nav, _| {
            read_release(nav, facts.at.clone(), now);
            finish_carry(nav, now, reduce_motion);
        });
        let nav_value = nav.read(cx).clone();
        let open_at = nav_value
            .open
            .clone()
            .and_then(|name| facts.modules.iter().position(|m| m.name == name));
        // Shingles carried to the cards of the module just opened: how far
        // they have come (1: not flying, or landed).
        let flying = nav_value.flight.clone();
        let carried = flying
            .as_ref()
            .filter(|flying| open_at.is_some_and(|open| facts.modules[open].name == flying.module))
            .map_or(1.0, |flying| progress(&flying.carry, cx));
        if !reduce_motion
            && flying
                .as_ref()
                .is_some_and(|flying| !flying.carry.settled(now, CARRY_REST))
        {
            request_frame(window, cx);
        }

        // The line above the territory: what the package makes public.
        let names: usize = facts.modules.iter().map(|m| m.items.len()).sum();
        let number = |name: &str, n: usize| {
            one(
                key(&self.id, name.to_owned()),
                n.to_string(),
                HEADER_NUMBER,
                palette.ink0,
                &measure,
            )
            .into_any_element()
        };
        let words = |name: &str, text: &str| {
            one(
                key(&self.id, name.to_owned()),
                text.to_owned(),
                HEADER,
                palette.ink2,
                &measure,
            )
            .into_any_element()
        };
        let plural = |n: usize, one_word: &str, many: &str| {
            if n == 1 {
                one_word.to_owned()
            } else {
                many.to_owned()
            }
        };
        // What the modules are, said as they are: a flat root and names the
        // index placed in no module are not drawn as though that were the
        // package's organisation.
        let counted: Vec<AnyElement> = match facts.structure {
            Structure::Modules => vec![
                number("names", names),
                words(
                    "names-words",
                    &plural(names, "public name in", "public names in"),
                ),
                number("modules", facts.modules.len()),
                words(
                    "modules-words",
                    &plural(facts.modules.len(), "module", "modules"),
                ),
            ],
            Structure::Root { hidden } => vec![
                number("names", names),
                words(
                    "names-words",
                    &plural(
                        names,
                        "public name, all at the root ·",
                        "public names, all at the root ·",
                    ),
                ),
                words("through", "re-exported from"),
                number("hidden", hidden),
                words(
                    "hidden-words",
                    &plural(hidden, "private module", "private modules"),
                ),
            ],
            Structure::Gathered => vec![
                number("names", names),
                words(
                    "names-words",
                    &plural(names, "public name ·", "public names ·"),
                ),
                words("gathered", "the index records no module for them"),
            ],
        };
        let header_line: AnyElement = div()
            .flex()
            .flex_wrap()
            .items_baseline()
            .gap_x(measure.space(Space::Snug))
            .children(counted)
            .when_some(facts.documented, |header, (with, total)| {
                header.child(one(
                    key(&self.id, "documented"),
                    format!(
                        "· documented {}%",
                        (with * 100).checked_div(total).unwrap_or(0)
                    ),
                    HEADER,
                    palette.ink2,
                    &measure,
                ))
            })
            .into_any_element();
        let header: AnyElement = match facts
            .outline_gap
            .clone()
            .filter(|_| facts.modules.is_empty())
        {
            Some(gap) => div()
                .flex()
                .flex_col()
                .gap(measure.space(Space::Snug))
                .child(one(
                    key(&self.id, "names-gap"),
                    "Its public names are not known yet",
                    HEADER,
                    palette.ink1,
                    &measure,
                ))
                .child(one(
                    key(&self.id, "names-why"),
                    gap,
                    HEADER,
                    palette.ink3,
                    &measure,
                ))
                .into_any_element(),
            None => header_line,
        };

        // What the keyboard walks, in this order: the territory (or the
        // module's cards), then the crest, the berg, the features, the releases.
        // The territory, or the module that is open.
        let territory: AnyElement = match open_at {
            None => {
                let state = nav.clone();
                let names: Vec<SharedString> =
                    facts.modules.iter().map(|m| m.name.clone()).collect();
                let items: Vec<Vec<SharedString>> = facts
                    .modules
                    .iter()
                    .map(|m| m.items.iter().map(|i| i.name.clone()).collect())
                    .collect();
                let geometry = facet::folio::shingles::measured(&modules, &measure, window);
                // The region the keyboard stands on reads itself, as a hovered one does.
                let standing = facts.modules.iter().position(|m| {
                    self.targets
                        .is_focused(&PageTarget::Module(m.name.clone()).id())
                });
                let carry_state = nav.clone();
                let carry_names = names.clone();
                let map = shingles(key(&self.id, "shingles"), modules.clone(), &measure)
                    .geometry(geometry.clone())
                    .time(if facts.past.is_some() {
                        Time::Past
                    } else {
                        Time::Now
                    })
                    .rest(standing.map(Spot::Region))
                    .on_carry(move |carrying, _window, cx| {
                        let Some(name) = carry_names.get(carrying.module).cloned() else {
                            return;
                        };
                        let now = facet::motion::now(cx);
                        carry_state.update(cx, |nav, _| {
                            begin_carry(nav, name, carrying.stones, now);
                        });
                    })
                    .on_open(move |module, item, _window, cx| {
                        let (Some(name), Some(items)) = (names.get(module), items.get(module))
                        else {
                            return;
                        };
                        let lit = item.and_then(|i| items.get(i)).cloned();
                        let now = facet::motion::now(cx);
                        state.update(cx, |nav, cx| {
                            request_module(nav, name.clone(), lit, now);
                            cx.notify();
                        });
                    });
                self.with_doors(map, &nav, &geometry)
            }
            Some(open) => self.open_module(open, &nav_value, &nav, &measure, carried),
        };
        // Esc folds the open module before the shell does anything of its own.
        if (open_at.is_some() || nav_value.pending.is_some()) && self.active {
            let state = nav.clone();
            self.targets.on_escape(Rc::new(move |_, cx| {
                let now = facet::motion::now(cx);
                state.update(cx, |nav, cx| {
                    close_module(nav, now);
                    cx.notify();
                });
            }));
        }

        // How many cells share a row is held by the page's own memory of its
        // modes; a change carries the cells to their new places.
        let modes = Modes::keyed(key(&self.id, "modes"), window, cx);
        let tracks = fluid::tracks(&measure, &modes);
        let flow = Flow::scoped("package-crest", cx);
        flow.epoch((tracks.epoch, facet.text_scale.to_bits()));
        let crest = self.crest(
            &tracks,
            &flow,
            &measure,
            &nav,
            nav_value.berg,
            nav_value.licence,
        );
        // The berg rises into the room it opens and lifts out again (the board's
        // 380 ms ease-out, no overshoot: everything under it moves with it);
        // only an open one is on the keyboard's list.
        let berg_items = Presence::scoped(format!("folio-berg-{}", self.id), cx)
            .enter(facet::motion::act::RISE)
            .exit(facet::motion::act::LEAVE)
            .sync(
                facts
                    .berg
                    .as_ref()
                    .filter(|_| nav_value.berg == Fold::Open)
                    .map(|_| "berg"),
                window,
                cx,
            );
        let berg_panel = facts.berg.as_ref().and_then(|facts_berg| {
            berg_items.into_iter().next().map(|item| {
                let live = !item.is_leaving();
                item.slot(self.berg_panel(facts_berg, &measure, live))
                    .into_any_element()
            })
        });
        let features_bar = facts
            .features
            .as_ref()
            .map(|facts_features| self.features_bar(facts_features, &measure));
        // The releases: a ticker that travels when pressed.
        let ticker_row = facts
            .ticker
            .as_ref()
            .filter(|t| t.ticks.len() > 1)
            .map(|ticks| self.ticker_row(ticks, &measure));
        let banner = facts
            .past
            .as_ref()
            .map(|past| self.banner(past, &measure, palette));
        let ticker_note = facts.ticker_note.as_ref().map(|note| {
            one(
                key(&self.id, "ticker-note"),
                note.clone(),
                ty::SMALL,
                palette.ink3,
                &measure,
            )
        });
        // A big module is a page of its own: the package's hero, crest and
        // features step aside, the ticker stays (it is where versions live),
        // and the module's rail and cards take the room.
        let dedicated = open_at.is_some_and(|open| facts.modules[open].extent() == Extent::Page);
        // The banner is not the ticker's: with no ticker the past still says so.
        let ticker_block = (ticker_row.is_some() || ticker_note.is_some() || banner.is_some())
            .then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap(measure.space(Space::Roomy))
                    .children(ticker_row)
                    .children(ticker_note)
                    .children(banner)
            });
        let column = div()
            .id(self.id.clone())
            .flex()
            .flex_col()
            .w(measure.width())
            .gap(measure.space(Space::Wide));
        // The shingles stay in the air while a close carries them back to the
        // map, even after the module's cards have left the tree.
        let in_the_air = flying
            .filter(|flying| !flying.carry.settled(now, CARRY_REST))
            .map(|flying| {
                flight(
                    key(&self.id, "flight"),
                    flying.stones,
                    nav_value.marks.clone(),
                    flying.carry,
                )
            });
        if dedicated {
            return column
                .children(ticker_block)
                .child(territory)
                .children(in_the_air);
        }
        // The blocks under the crest move when its cells change rows (four
        // in a row, two by two): they are items of the crest's flow, so they
        // glide to where the new arrangement puts them instead of jumping
        // the crest's change of height in one frame (FLUID-C). Between the
        // crest's changes a window drag moves them with no lag.
        let blocks = Flow::scoped("package-blocks", cx).only_epochs();
        blocks.epoch((tracks.epoch, facet.text_scale.to_bits()));
        let block = |name: &str, element: AnyElement| {
            blocks.item(key(&self.id, format!("flow-block-{name}")), element)
        };
        column
            .child(self.hero)
            .child(crest)
            .children(berg_panel)
            .children(ticker_block.map(|ticker| block("ticker", ticker.into_any_element())))
            .children(features_bar.map(|bar| block("features", bar)))
            .child(block(
                "territory",
                div()
                    .flex()
                    .flex_col()
                    .gap(measure.space(Space::Roomy))
                    .child(header)
                    .child(territory)
                    .into_any_element(),
            ))
            .children(in_the_air)
    }
}

impl Folio {
    /// The map with a keyboard door on each region: `j` and `k` walk the
    /// modules and Enter opens the one focused, the way a pointer does.
    /// The doors are invisible; the region itself is what paints.
    fn with_doors(
        &self,
        map: facet::folio::shingles::Shingles,
        nav: &Entity<Nav>,
        geometry: &facet::folio::shingles::Layout,
    ) -> AnyElement {
        let over = geometry
            .region_bounds()
            .into_iter()
            .zip(self.facts.modules.iter())
            .map(|(at, module)| {
                let (state, name) = (nav.clone(), module.name.clone());
                let act: Act = Rc::new(move |_, cx| {
                    let now = facet::motion::now(cx);
                    state.update(cx, |nav, cx| {
                        request_module(nav, name.clone(), None, now);
                        cx.notify();
                    });
                });
                Door {
                    target: PageTarget::Module(module.name.clone()),
                    label: module.name.clone(),
                    at,
                    act,
                }
            })
            .collect();
        doors(&self.targets, self.active, map, over)
    }

    /// The weight berg opened: a block is a door to the package it stands for.
    fn berg_panel(&self, facts_berg: &Rc<BergFacts>, measure: &Measure, live: bool) -> AnyElement {
        let go = {
            let (links, blocks) = (self.links.clone(), facts_berg.blocks.clone());
            move |index: usize, cx: &mut App| {
                if let Some(block) = blocks.get(index)
                    && let Ok(package) =
                        PackageRef::parse(&format!("pkg:cargo/{}@{}", block.name, block.version))
                    && let Some(route) = package_route(&package)
                {
                    links.dispatch(Intent::Navigate(route), cx);
                }
            }
        };
        let standing = (0..facts_berg.blocks.len())
            .find(|index| self.targets.is_focused(&PageTarget::Block(*index).id()));
        let click = go.clone();
        let element = berg_view(key(&self.id, "berg"), facts_berg.clone(), measure)
            .rest(standing)
            .on_go(move |index, _window, cx| click(index, cx));
        let over = facet::folio::berg::doors(facts_berg, measure)
            .into_iter()
            .enumerate()
            .map(|(index, at)| {
                let go = go.clone();
                let act: Act = Rc::new(move |_, cx| go(index, cx));
                Door {
                    target: PageTarget::Block(index),
                    label: facts_berg.blocks[index].name.clone(),
                    at,
                    act,
                }
            })
            .collect();
        doors(&self.targets, self.active && live, element, over)
    }

    /// A read-only view of the manifest's default feature profile.
    fn features_bar(&self, facts_features: &Rc<FeatureFacts>, measure: &Measure) -> AnyElement {
        feature_preview(
            key(&self.id, "features"),
            facts_features.clone(),
            measure.width(),
            measure,
        )
        .into_any_element()
    }

    /// The release ticker: pin, the release being read, the newest and every
    /// release that broke its API are doors, Enter travels to it.
    fn ticker_row(&self, ticks: &Rc<TickerFacts>, measure: &Measure) -> AnyElement {
        let travel = {
            let (ticks, links, pin) = (ticks.clone(), self.links.clone(), self.facts.pin.clone());
            move |index: usize, cx: &mut App| {
                let Some(tick) = ticks.ticks.get(index) else {
                    return;
                };
                let is_pin = pin
                    .as_deref()
                    .is_some_and(|pin| pin == tick.version.as_ref());
                let at = if is_pin {
                    None
                } else {
                    crate::navigation::ReleaseId::new(&tick.version).ok()
                };
                links.dispatch(Intent::SetRelease(at), cx);
            }
        };
        let tick_of = |mark: Mark| match mark {
            Mark::Pin => ticks.pin,
            Mark::Reading => ticks.reading,
            Mark::Newest => ticks.latest,
            Mark::Breaking(index) => Some(index),
        };
        let mut marks: Vec<Mark> = [Mark::Pin, Mark::Reading, Mark::Newest]
            .into_iter()
            .filter(|mark| tick_of(*mark).is_some())
            .collect();
        marks.extend(
            ticks
                .ticks
                .iter()
                .enumerate()
                .filter(|(index, tick)| {
                    tick.kind == facet::marks::semver::Tick::Breaking
                        && [ticks.pin, ticks.reading, ticks.latest]
                            .iter()
                            .all(|other| *other != Some(*index))
                })
                .map(|(index, _)| Mark::Breaking(index)),
        );
        let standing = marks
            .iter()
            .find(|mark| self.targets.is_focused(&PageTarget::Release(**mark).id()))
            .and_then(|mark| tick_of(*mark));
        let click = travel.clone();
        let element = ticker(key(&self.id, "ticker"), ticks.clone(), measure)
            .stand(standing)
            .on_travel(move |index, _window, cx| click(index, cx));
        let over = marks
            .into_iter()
            .filter_map(|mark| {
                let index = tick_of(mark)?;
                let at = facet::folio::ticker::door(ticks, measure, index)?;
                let travel = travel.clone();
                let act: Act = Rc::new(move |_, cx| travel(index, cx));
                let label: SharedString = match mark {
                    Mark::Pin => format!("your pin {}", ticks.ticks[index].version),
                    Mark::Reading => format!("reading {}", ticks.ticks[index].version),
                    Mark::Newest => format!("newest {}", ticks.ticks[index].version),
                    Mark::Breaking(_) => ticks.ticks[index].version.to_string(),
                }
                .into();
                Some(Door {
                    target: PageTarget::Release(mark),
                    label,
                    at,
                    act,
                })
            })
            .collect();
        doors(&self.targets, self.active, element, over)
    }

    /// The module at `open`: a rail to the others, then its cards (a page
    /// of its own when it is big).
    fn open_module(
        &self,
        open: usize,
        current: &Nav,
        nav: &Entity<Nav>,
        measure: &Measure,
        carried: f32,
    ) -> AnyElement {
        let facts = &self.facts;
        let module = &facts.modules[open];
        let past = facts.past.as_ref();
        let cards: Vec<Rc<CardFacts>> = module
            .items
            .iter()
            .map(|item| {
                let reading = Item::new(&item.name, item.lang)
                    .kind(Some(item.kind))
                    .signature(item.signature.as_deref());
                Rc::new(
                    CardFacts::of(&reading, item.summary.as_deref())
                        .yours(Use::Elsewhere)
                        .change(past.and_then(|past| past.states.get(&item.name).copied())),
                )
            })
            .collect();
        let lit = current
            .lit
            .as_ref()
            .and_then(|lit| module.items.iter().position(|item| &item.name == lit));
        let chips: Vec<(SharedString, usize)> = facts
            .modules
            .iter()
            .map(|m| (m.name.clone(), m.items.len()))
            .collect();
        let pick_state = nav.clone();
        let pick_names: Vec<SharedString> = facts.modules.iter().map(|m| m.name.clone()).collect();
        let close_state = nav.clone();
        let rail = rail(
            key(&self.id, "rail"),
            chips,
            if module.extent() == Extent::Page {
                format!("back to {}", facts.name)
            } else {
                "fold".to_owned()
            },
            measure,
        )
        .current(Some(open))
        .on_pick(move |index, _window, cx| {
            if let Some(name) = pick_names.get(index) {
                let now = facet::motion::now(cx);
                pick_state.update(cx, |nav, cx| {
                    request_module(nav, name.clone(), None, now);
                    cx.notify();
                });
            }
        })
        .on_close(move |_window, cx| {
            let now = facet::motion::now(cx);
            close_state.update(cx, |nav, cx| {
                close_module(nav, now);
                cx.notify();
            });
        });
        let links = self.links.clone();
        let targets = self.targets.clone();
        let active = self.active;
        let route_state = nav.clone();
        let package = self.package.clone();
        let symbols: Vec<crate::model::pages::SymbolRef> =
            module.items.iter().map(|i| i.symbol.clone()).collect();
        let open_symbols = symbols.clone();
        let open_links = links.clone();
        let recall = self.recall.clone();
        let click_recall = recall.clone();
        let package_text = package.as_str().to_owned();
        let name_words: Vec<SharedString> = module.items.iter().map(|i| i.name.clone()).collect();
        // The cards are doors, registered now (in walk order: before the
        // crest's cells, which the module view would otherwise follow).
        if active {
            for (index, symbol) in symbols.iter().enumerate() {
                let leave_id = PageTarget::Card(symbol.clone()).id();
                let act: Act = {
                    let (links, package_text, symbol, recall, leave_id, route_state) = (
                        links.clone(),
                        package_text.clone(),
                        symbol.clone(),
                        recall.clone(),
                        leave_id.clone(),
                        route_state.clone(),
                    );
                    Rc::new(move |_, cx| {
                        // The page is left by this card: Back lands on it again.
                        let leaving = links.snapshot(cx).route().clone();
                        recall.focus(leave_id.clone());
                        recall.remember_leave(leaving, leave_id.clone());
                        if let Some(route) = symbol_route(&package_text, &symbol) {
                            let now = facet::motion::now(cx);
                            route_state.update(cx, |nav, cx| {
                                leave_for_symbol(nav, now);
                                cx.notify();
                            });
                            links.dispatch(Intent::Navigate(route), cx);
                        }
                    })
                };
                targets.push(Target {
                    id: leave_id,
                    label: name_words.get(index).cloned().unwrap_or_default(),
                    act,
                    peek: Some(PageKey::Symbol(symbol.clone())),
                    source: Some(symbol.clone()),
                });
            }
        }
        let view = module_view(
            key(&self.id, "module"),
            facts.name.clone(),
            module.name.clone(),
            cards,
            measure,
        )
        .doc(module.doc.clone())
        .extent(module.extent())
        .marks(&current.marks)
        .carried(carried)
        .at(past.map(|p| p.at.clone()).unwrap_or_default())
        .lit(lit)
        .on_open({
            let package_text = package_text.clone();
            let route_state = route_state.clone();
            move |index, _window, cx| {
                if let Some(symbol) = open_symbols.get(index)
                    && let Some(route) = symbol_route(&package_text, symbol)
                {
                    let id = PageTarget::Card(symbol.clone()).id();
                    let leaving = open_links.snapshot(cx).route().clone();
                    click_recall.focus(id.clone());
                    click_recall.remember_leave(leaving, id);
                    let now = facet::motion::now(cx);
                    route_state.update(cx, |nav, cx| {
                        leave_for_symbol(nav, now);
                        cx.notify();
                    });
                    open_links.dispatch(Intent::Navigate(route), cx);
                }
            }
        })
        .wrap(move |index, card| {
            let Some(symbol) = symbols.get(index).cloned() else {
                return card;
            };
            let id = PageTarget::Card(symbol.clone()).id();
            let (hover_links, warm) = (links.clone(), PageKey::Symbol(symbol));
            let wrapped = div()
                .id(SharedString::from(format!("{id}-hover")))
                .on_hover(move |hovered: &bool, _window, cx| {
                    if *hovered {
                        hover_links.prefetch(warm.clone(), cx);
                    } else {
                        hover_links.cancel_prefetch(&warm, cx);
                    }
                })
                .child(card);
            targets.track(id, wrapped).into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .gap(measure.space(Space::Wide))
            .child(rail)
            .child(view)
            .into_any_element()
    }
}
