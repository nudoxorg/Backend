//! The package folio: the hero's crest, the release ticker, and the
//! territory of shingles that opens into cards. This is the component that
//! remembers which module is open; everything it draws comes from `facet`.

use super::data::{Diffs, ModuleData, Past, Side};
use super::fluid::{self, Tracks};
use crate::navigation::Intent;
use crate::shell::focus::{Act, Recall, Target, Targets};
use crate::shell::kit::{package_route, symbol_route};
use crate::shell::region::Links;
use crate::model::pages::{PackageRef, PageKey};
use crate::model::source_facts::Reading;
use facet::folio::berg::{BergFacts, berg as berg_view, weight};
use facet::folio::cards::CardFacts;
use facet::folio::crest::{self, Advisories, REST};
use facet::folio::features::{FeatureFacts, features};
use facet::folio::heads::{Finding, heads};
use facet::folio::module::module as module_view;
use facet::folio::rail::rail;
use facet::fluid::Modes;
use facet::motion::Flow;
use facet::motion::flow::FlowItem;
use facet::tokens::fluid::Crest;
use facet::folio::shingles::{ModuleFacts, ShingleFacts, shingles};
use facet::folio::state::{Extent, Fold, Time, Use};
use facet::folio::text::{key, one};
use facet::folio::ticker::{TickerFacts, ticker};
use facet::marks::badges::{Glyph, Item, glyph};
use facet::marks::license::LicenseFacts;
use facet::ActiveFacet as _;
use facet::tokens::{TypeRole, ty};
use facet::{Measure, Space};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ElementId, Entity, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use std::rc::Rc;

/// What the folio remembers between frames.
#[derive(Clone, Debug, Default)]
struct Nav {
    /// The module open, by name.
    open: Option<SharedString>,
    /// The name a click on a shingle asked for.
    lit: Option<SharedString>,
    /// Whether the full berg is showing.
    berg: Fold,
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

const HEADER: TypeRole = TypeRole { size: 13.0, line: 18.0, ..ty::SMALL };
const HEADER_NUMBER: TypeRole = TypeRole { weight: 600.0, size: 13.0, line: 18.0, ..ty::MONO_SMALL };
const BANNER_WORD: TypeRole = TypeRole { size: 12.5, line: 18.0, ..ty::SMALL };
const BANNER_VERSION: TypeRole = TypeRole { weight: 600.0, size: 12.5, line: 18.0, ..ty::MONO_SMALL };

impl Folio {
    fn state(&self, window: &mut Window, cx: &mut App) -> Entity<Nav> {
        let reopen = self.reopen.clone();
        window.use_keyed_state(self.id.clone(), cx, move |_, _| Nav { open: reopen, ..Nav::default() })
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
    fn crest(&self, tracks: &Tracks, flow: &Flow, measure: &Measure, nav: &Entity<Nav>, berg_open: Fold) -> AnyElement {
        let (stamp_w, cell_w, spread) = (tracks.stamp, tracks.cell, tracks.spread);
        let source_words = |what: &str| -> SharedString {
            match &self.facts.source {
                Reading::Reading => format!("Reading its source on disk for {what}.").into(),
                Reading::Absent(why) => format!("{why} Nothing is read for {what}.").into(),
                Reading::Ready(_) => SharedString::default(),
            }
        };
        let heads_cell = match &self.facts.heads {
            Some(findings) => heads(key(&self.id, "heads"), self.facts.name.clone(), findings.clone(), cell_w, spread, REST, measure).into_any_element(),
            None => crest::unread(key(&self.id, "heads"), "Heads-up", source_words("build scripts, network, files and unsafe"), cell_w, measure).into_any_element(),
        };
        let weight_cell = match &self.facts.berg {
            Some(facts) => {
                let state = nav.clone();
                weight(key(&self.id, "weight"), facts.clone(), cell_w, REST, berg_open, measure)
                    .on_toggle(move |_window, cx| {
                        state.update(cx, |nav, cx| {
                            nav.berg = if nav.berg == Fold::Open { Fold::Folded } else { Fold::Open };
                            cx.notify();
                        });
                    })
                    .into_any_element()
            }
            None => crest::unread(key(&self.id, "weight"), "Weight", source_words("lines of code"), cell_w, measure).into_any_element(),
        };
        let cell = |name: &str, element: AnyElement| flow.item(key(&self.id, format!("flow-{name}")), element);
        let cells = [
            cell("licence", crest::stamp(key(&self.id, "licence"), self.facts.licence.clone(), stamp_w, measure).into_any_element()),
            cell("heads", heads_cell),
            cell("weight", weight_cell),
            cell("advisories", crest::advisories(key(&self.id, "advisories"), self.facts.advisories.clone(), cell_w, measure).into_any_element()),
        ];
        // The rows are the arrangement's, named, never left to a wrap: the
        // cells sum to their row's width, and a wrap would drop the last one
        // onto a row of its own at the first pixel of error.
        let row = |cells: Vec<FlowItem>| div().flex().items_start().gap(tracks.gap).children(cells);
        let mut cells = cells.into_iter();
        match tracks.crest {
            Crest::Four => row(cells.collect()).into_any_element(),
            Crest::Two => {
                let (first, second): (Vec<_>, Vec<_>) = cells.enumerate().partition(|(index, _)| *index < 2);
                div()
                    .flex()
                    .flex_col()
                    .gap(tracks.gap)
                    .child(row(first.into_iter().map(|(_, cell)| cell).collect()))
                    .child(row(second.into_iter().map(|(_, cell)| cell).collect()))
                    .into_any_element()
            }
            Crest::One => div().flex().flex_col().gap(tracks.gap).children(cells.by_ref().map(|cell| row(vec![cell]))).into_any_element(),
        }
    }

    /// The banner that says the page is in the past.
    fn banner(&self, past: &Past, measure: &Measure, palette: &'static facet::Palette) -> AnyElement {
        let words = if past.diffs == Diffs::Known {
            if past.side == Side::Before {
                format!(
                    "{} of today's names did not exist yet · {} looked different",
                    past.absent, past.changed
                )
            } else {
                format!("{} new names · {} changed · {} gone", past.added, past.changed, past.gone)
            }
        } else {
            "Its names are not read yet: only the release's date and size are known.".to_owned()
        };
        let relation = if past.side == Side::Before { "before" } else { "after" };
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
                    .child(glyph(Glyph::Marker, 12.0 * measure.scale(), palette.amber.base))
                    .child(one(key(&self.id, "reading"), "Reading", BANNER_WORD, palette.ink1, measure))
                    .child(one(key(&self.id, "at"), past.at.clone(), BANNER_VERSION, palette.amber.base, measure))
                    .child(one(key(&self.id, "relation"), format!("{relation} your pin"), BANNER_WORD, palette.ink1, measure)),
            )
            .child(one(key(&self.id, "words"), words, BANNER_WORD, palette.ink1, measure))
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

impl RenderOnce for Folio {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let measure = self.measure;
        let facts = self.facts.clone();
        let nav = self.state(window, cx);
        let modules = self.map_modules();
        let nav_value = nav.read(cx).clone();
        let open_at = nav_value.open.clone().and_then(|name| facts.modules.iter().position(|m| m.name == name));

        // The releases: a ticker that travels when pressed.
        let ticker_row = facts.ticker.as_ref().filter(|t| t.ticks.len() > 1).map(|ticks| {
            let ticks_for_travel = ticks.clone();
            let links = self.links.clone();
            let pin = facts.pin.clone();
            ticker(key(&self.id, "ticker"), ticks.clone(), &measure)
                .on_travel(move |index, _window, cx| {
                    let Some(tick) = ticks_for_travel.ticks.get(index) else { return };
                    let is_pin = pin.as_deref().is_some_and(|pin| pin == tick.version.as_ref());
                    let at = if is_pin { None } else { crate::navigation::ReleaseId::new(&tick.version).ok() };
                    links.dispatch(Intent::SetRelease(at), cx);
                })
                .into_any_element()
        });

        // The line above the territory: what the package makes public.
        let names: usize = facts.modules.iter().map(|m| m.items.len()).sum();
        let header_line: AnyElement = div()
            .flex()
            .flex_wrap()
            .items_baseline()
            .gap_x(measure.space(Space::Snug))
            .child(one(key(&self.id, "names"), names.to_string(), HEADER_NUMBER, palette.ink0, &measure))
            .child(one(key(&self.id, "names-words"), if names == 1 { "public name in" } else { "public names in" }, HEADER, palette.ink2, &measure))
            .child(one(key(&self.id, "modules"), facts.modules.len().to_string(), HEADER_NUMBER, palette.ink0, &measure))
            .child(one(key(&self.id, "modules-words"), if facts.modules.len() == 1 { "module" } else { "modules" }, HEADER, palette.ink2, &measure))
            .when_some(facts.documented, |header, (with, total)| {
                header.child(one(
                    key(&self.id, "documented"),
                    format!("· documented {}%", (with * 100).checked_div(total).unwrap_or(0)),
                    HEADER,
                    palette.ink2,
                    &measure,
                ))
            }).into_any_element();
        let header: AnyElement = match facts.outline_gap.clone().filter(|_| facts.modules.is_empty()) {
            Some(gap) => div()
                .flex()
                .flex_col()
                .gap(measure.space(Space::Snug))
                .child(one(key(&self.id, "names-gap"), "Its public names are not known yet", HEADER, palette.ink1, &measure))
                .child(one(key(&self.id, "names-why"), gap, HEADER, palette.ink3, &measure))
                .into_any_element(),
            None => header_line,
        };

        // The territory, or the module that is open.
        let territory: AnyElement = match open_at {
            None => {
                let state = nav.clone();
                let names: Vec<SharedString> = facts.modules.iter().map(|m| m.name.clone()).collect();
                let items: Vec<Vec<SharedString>> = facts.modules.iter().map(|m| m.items.iter().map(|i| i.name.clone()).collect()).collect();
                let map = shingles(key(&self.id, "shingles"), modules.clone(), &measure)
                    .time(if facts.past.is_some() { Time::Past } else { Time::Now })
                    .on_open(move |module, item, _window, cx| {
                        let (Some(name), Some(items)) = (names.get(module), items.get(module)) else { return };
                        let lit = item.and_then(|i| items.get(i)).cloned();
                        state.update(cx, |nav, cx| {
                            nav.open = Some(name.clone());
                            nav.lit = lit;
                            cx.notify();
                        });
                    });
                self.with_doors(map, &modules, &nav, &measure)
            }
            Some(open) => self.open_module(open, &nav_value, &nav, &measure),
        };

        // How many cells share a row is held by the page's own memory of its
        // modes; a change carries the cells to their new places.
        let modes = Modes::keyed(key(&self.id, "modes"), window, cx);
        let tracks = fluid::tracks(&measure, &modes);
        let flow = Flow::scoped("package-crest", cx);
        flow.epoch((tracks.epoch, facet.text_scale.to_bits()));
        let crest = self.crest(&tracks, &flow, &measure, &nav, nav_value.berg);
        let berg_panel = facts.berg.as_ref().filter(|_| nav_value.berg == Fold::Open).map(|facts_berg| {
            let links = self.links.clone();
            let blocks = facts_berg.blocks.clone();
            berg_view(key(&self.id, "berg"), facts_berg.clone(), &measure)
                .on_go(move |index, _window, cx| {
                    if let Some(block) = blocks.get(index)
                        && let Some(package) = PackageRef::parse(&format!("pkg:cargo/{}@{}", block.name, block.version)).ok()
                        && let Some(route) = package_route(&package)
                    {
                        links.dispatch(Intent::Navigate(route), cx);
                    }
                })
                .into_any_element()
        });
        let features_bar = facts.features.as_ref().map(|facts_features| features(key(&self.id, "features"), facts_features.clone(), measure.width(), &measure).into_any_element());
        let banner = facts.past.as_ref().map(|past| self.banner(past, &measure, palette));
        // A big module is a page of its own: the package's hero, crest and
        // features step aside, the ticker stays (it is where versions live),
        // and the module's rail and cards take the room.
        let dedicated = open_at.is_some_and(|open| facts.modules[open].extent() == Extent::Page);
        // The banner is not the ticker's: with no ticker the past still says so.
        let ticker_block = (ticker_row.is_some() || banner.is_some()).then(|| div().flex().flex_col().gap(measure.space(Space::Roomy)).children(ticker_row).children(banner));
        let column = div().id(self.id.clone()).flex().flex_col().w(measure.width()).gap(measure.space(Space::Wide));
        if dedicated {
            return column.children(ticker_block).child(territory);
        }
        column
            .child(self.hero)
            .child(crest)
            .children(berg_panel)
            .children(ticker_block)
            .children(features_bar)
            .child(div().flex().flex_col().gap(measure.space(Space::Roomy)).child(header).child(territory))
    }
}

impl Folio {
    /// The map with a keyboard door on each region: `j` and `k` walk the
    /// modules and Enter opens the one focused, the way a pointer does.
    /// The doors are invisible; the region itself is what paints.
    fn with_doors(&self, map: facet::folio::shingles::Shingles, modules: &[ModuleFacts], nav: &Entity<Nav>, measure: &Measure) -> AnyElement {
        let rects = facet::folio::shingles::rects(modules, measure);
        let mut holder = div().relative().child(map);
        for ((x, y, w, h), module) in rects.into_iter().zip(self.facts.modules.iter()) {
            let id: SharedString = format!("pkg-module-{}", module.name).into();
            if self.active {
                let (state, name) = (nav.clone(), module.name.clone());
                let act: Act = Rc::new(move |_, cx| {
                    state.update(cx, |nav, cx| {
                        nav.open = Some(name.clone());
                        nav.lit = None;
                        cx.notify();
                    });
                });
                self.targets.push(Target { id: id.clone(), label: module.name.clone(), act, peek: None, source: None });
            }
            let door = div().absolute().left(px(x)).top(px(y)).w(px(w)).h(px(h));
            holder = holder.child(self.targets.track(id, door));
        }
        holder.into_any_element()
    }

    /// The module at `open`: a rail to the others, then its cards (a page
    /// of its own when it is big).
    fn open_module(&self, open: usize, current: &Nav, nav: &Entity<Nav>, measure: &Measure) -> AnyElement {
        let facts = &self.facts;
        let module = &facts.modules[open];
        let past = facts.past.as_ref();
        let cards: Vec<Rc<CardFacts>> = module
            .items
            .iter()
            .map(|item| {
                let reading = Item::new(&item.name, item.lang).kind(Some(item.kind)).signature(item.signature.as_deref());
                Rc::new(
                    CardFacts::of(&reading, item.summary.as_deref())
                        .yours(Use::Elsewhere)
                        .change(past.and_then(|past| past.states.get(&item.name).copied())),
                )
            })
            .collect();
        let lit = current.lit.as_ref().and_then(|lit| module.items.iter().position(|item| &item.name == lit));
        let chips: Vec<(SharedString, usize)> = facts.modules.iter().map(|m| (m.name.clone(), m.items.len())).collect();
        let pick_state = nav.clone();
        let pick_names: Vec<SharedString> = facts.modules.iter().map(|m| m.name.clone()).collect();
        let close_state = nav.clone();
        let rail = rail(key(&self.id, "rail"), chips, if module.extent() == Extent::Page { format!("back to {}", facts.name) } else { "fold".to_owned() }, measure)
            .current(Some(open))
            .on_pick(move |index, _window, cx| {
                if let Some(name) = pick_names.get(index) {
                    pick_state.update(cx, |nav, cx| {
                        nav.open = Some(name.clone());
                        nav.lit = None;
                        cx.notify();
                    });
                }
            })
            .on_close(move |_window, cx| {
                close_state.update(cx, |nav, cx| {
                    nav.open = None;
                    nav.lit = None;
                    cx.notify();
                });
            });
        let links = self.links.clone();
        let targets = self.targets.clone();
        let active = self.active;
        let package = self.package.clone();
        let symbols: Vec<crate::model::pages::SymbolRef> = module.items.iter().map(|i| i.symbol.clone()).collect();
        let open_symbols = symbols.clone();
        let open_links = links.clone();
        let recall = self.recall.clone();
        let click_recall = recall.clone();
        let package_text = package.as_str().to_owned();
        let name_words: Vec<SharedString> = module.items.iter().map(|i| i.name.clone()).collect();
        let view = module_view(key(&self.id, "module"), facts.name.clone(), module.name.clone(), cards, measure)
            .doc(module.doc.clone())
            .extent(module.extent())
            .at(past.map(|p| p.at.clone()).unwrap_or_default())
            .lit(lit)
            .on_open({
                let package_text = package_text.clone();
                move |index, _window, cx| {
                    if let Some(symbol) = open_symbols.get(index)
                        && let Some(route) = symbol_route(&package_text, symbol)
                    {
                        let id: SharedString = format!("pkg-card-{}", symbol.as_str()).into();
                        let leaving = open_links.snapshot(cx).route().clone();
                        click_recall.focus(id.clone());
                        click_recall.remember_leave(leaving, id);
                        open_links.dispatch(Intent::Navigate(route), cx);
                    }
                }
            })
            .wrap(move |index, card| {
                let Some(symbol) = symbols.get(index).cloned() else { return card };
                let id: SharedString = format!("pkg-card-{}", symbol.as_str()).into();
                let act: Act = {
                    let (links, package_text, symbol, recall, leave_id) = (links.clone(), package_text.clone(), symbol.clone(), recall.clone(), id.clone());
                    Rc::new(move |_, cx| {
                        // The page is left by this card: Back lands on it again.
                        let leaving = links.snapshot(cx).route().clone();
                        recall.focus(leave_id.clone());
                        recall.remember_leave(leaving, leave_id.clone());
                        if let Some(route) = symbol_route(&package_text, &symbol) {
                            links.dispatch(Intent::Navigate(route), cx);
                        }
                    })
                };
                if active {
                    targets.push(Target {
                        id: id.clone(),
                        label: name_words.get(index).cloned().unwrap_or_default(),
                        act,
                        peek: Some(PageKey::Symbol(symbol.clone())),
                        source: Some(symbol.clone()),
                    });
                }
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
        div().flex().flex_col().gap(measure.space(Space::Wide)).child(rail).child(view).into_any_element()
    }
}
