//! The reader region: the page, laid out from its own measured width.
//!
//! The folio holds the page at a reading measure (784 px at 100 % text) and
//! centres it; a body's margin notes sit in a 250 px column beside their
//! block when the reader's room is wide, and fold under their block
//! otherwise.
//!
//! Every arrival is a keyed [`Presence`] item: the new page enters (deeper
//! pages rise into place, shallower ones settle from above, sideways ones
//! slide, a view switch crossfades) while the page it replaces *leaves* —
//! drawn from its own route, inert, over the new one — instead of vanishing
//! in one frame. A second arrival mid-flight retargets from wherever each
//! page is; reduced motion snaps.

use super::bodies::{self, Ctx, Lens, Pages};
use super::focus::Targets;
use super::kit::HoverIntent;
use super::region::{Links, Region, RegionCore};
use super::thread::{route_package, route_symbol};
use crate::model::AppSnapshot;
use crate::model::pages::PageKey;
use crate::navigation::{Overlay, Route, View};
use crate::runtime::store::{Branch, DataStore, StoreEvent};
use facet::motion::presence::{Act, Entry, Extent};
use facet::motion::{Keys, Motion, Pose, Presence};
use facet::tokens::motion;
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Room, Space};
use gpui::{
    Context, ElementId, InteractiveElement, IntoElement, ParentElement, Pixels, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, Window, div, point, px,
};

/// The reading measure at 100 % text.
pub(crate) const FOLIO: f32 = 784.0;
/// The margin column at 100 % text.
pub(crate) const MARGIN: f32 = 250.0;
/// The gutter between folio and margin at 100 % text.
pub(crate) const GUTTER: f32 = 34.0;

/// Which way the last route change moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Way {
    /// Deeper: the page rises.
    Down,
    /// Shallower: the page settles from above.
    Up,
    /// Same depth: the page slides in.
    Across,
    /// The same declaration another way: a crossfade (W-Flow's shared
    /// elements morph the marks that carry the same id).
    View,
}

/// The reader region.
pub(crate) struct Reader {
    core: RegionCore,
    links: Links,
    pub(crate) targets: Targets,
    motion: Motion,
    hover: HoverIntent,
    scroll: ScrollHandle,
    lens: Lens,
    route: Route,
    overlay: Option<Overlay>,
    /// Route changes seen (each arrival's key).
    descents: u64,
    last_way: Option<Way>,
    /// Every page on screen: the current one, and any still leaving.
    pages: Presence,
    places: Vec<Place>,
    /// Every string the last render put on screen, in order.
    said: Vec<SharedString>,
    /// The hero name's lines as the last render set them.
    hero: Vec<SharedString>,
}

impl Reader {
    pub(crate) fn new(links: Links, store: &DataStore) -> Self {
        let snapshot = store.snapshot();
        Self {
            core: RegionCore::new(
                store,
                &[Branch::Route, Branch::Overlay, Branch::Workspace, Branch::Settings],
            ),
            links,
            targets: Targets::default(),
            motion: Motion::new(),
            hover: HoverIntent::default(),
            scroll: ScrollHandle::new(),
            lens: Lens::Reference,
            route: snapshot.route().clone(),
            overlay: snapshot.overlay(),
            descents: 0,
            last_way: None,
            pages: Presence::new("reader.pages"),
            places: vec![Place {
                key: 0,
                route: snapshot.route().clone(),
                overlay: snapshot.overlay().filter(|overlay| matches!(overlay, Overlay::Settings(_) | Overlay::Inbox)),
                way: Way::Across,
                lens: Lens::Reference,
            }],
            said: Vec::new(),
            hero: Vec::new(),
        }
    }

    pub(crate) const fn renders(&self) -> u64 {
        self.core.renders()
    }

    /// The strings the last render put on screen.
    pub(crate) fn said(&self) -> &[SharedString] {
        &self.said
    }

    /// The hero name's lines as the last render set them.
    pub(crate) fn hero(&self) -> &[SharedString] {
        &self.hero
    }

    /// Pages drawn by the last render (the current one plus any leaving).
    pub(crate) fn pages_on_screen(&self) -> usize {
        self.places.len()
    }

    /// How many descents played, and which way the last one went.
    pub(crate) const fn descent(&self) -> (u64, Option<Way>) {
        (self.descents, self.last_way)
    }

    pub(crate) fn set_lens(&mut self, lens: Lens, cx: &mut Context<Self>) {
        if self.lens != lens {
            self.lens = lens;
            cx.notify();
        }
    }

    /// A link was entered or left: hover intent may prefetch its page.
    pub(crate) fn hover_link(&mut self, key: PageKey, hovered: bool, cx: &mut Context<Self>) {
        let links = self.links.clone();
        self.hover.hover(key, hovered, &links, cx);
    }

    /// Keeps the focused target on screen after a keyboard walk.
    pub(crate) fn reveal_focused(&self) {
        let (Some(target), Some(view)) = (self.targets.focused_bounds(), Some(self.scroll.bounds())) else {
            return;
        };
        let offset = self.scroll.offset();
        let margin = px(48.0);
        if target.origin.y < view.origin.y + margin {
            let delta = view.origin.y + margin - target.origin.y;
            self.scroll.set_offset(point(offset.x, (offset.y + delta).min(px(0.0))));
        } else if target.origin.y + target.size.height > view.origin.y + view.size.height - margin {
            let delta = target.origin.y + target.size.height - (view.origin.y + view.size.height - margin);
            self.scroll.set_offset(point(offset.x, offset.y - delta));
        }
    }

    fn arrive(&mut self, next: &Route, overlay: Option<Overlay>) {
        let view_switch = overlay == self.overlay && self.route.same_place(next);
        let way = if view_switch {
            Way::View
        } else {
            match (self.route.depth(), next.depth()) {
                (Some(from), Some(to)) if to > from => Way::Down,
                (Some(from), Some(to)) if to < from => Way::Up,
                _ => Way::Across,
            }
        };
        let way = if overlay != self.overlay && self.route == *next { Way::Across } else { way };
        if let Some(current) = self.places.last_mut() {
            current.lens = self.lens;
        }
        self.route = next.clone();
        self.overlay = overlay;
        self.descents = self.descents.wrapping_add(1);
        self.last_way = Some(way);
        self.places.push(Place {
            key: self.descents,
            route: next.clone(),
            overlay,
            way,
            lens: Lens::Reference,
        });
        if !view_switch {
            self.lens = Lens::Reference;
            self.scroll.set_offset(point(px(0.0), px(0.0)));
        }
        self.targets.clear_focus();
    }

    /// Lays a body's leaves out: notes beside their block when wide,
    /// under it otherwise.
    fn compose(leaves: Vec<bodies::Leaf>, layout: &Layout, palette: &facet::Palette) -> gpui::Div {
        let Layout {
            folio,
            beside,
            content,
            wide,
            gutter,
            margin,
            measure,
            folio_measure,
        } = *layout;
        let gap = folio_measure.space(Space::Wide);
        let mut column = div().flex().flex_col().gap(gap).w(folio + beside).max_w(content);
        for leaf in leaves {
            column = column.child(match (leaf.note, wide) {
                (Some(note), true) => div()
                    .flex()
                    .items_start()
                    .child(div().w(folio).flex_none().child(leaf.main))
                    .child(div().w(gutter).flex_none())
                    .child(
                        div()
                            .w(margin)
                            .flex_none()
                            .pl(measure.space(Space::Roomy))
                            .border_l_1()
                            .border_color(palette.line1.hsla())
                            .child(note),
                    )
                    .into_any_element(),
                (Some(note), false) => div()
                    .flex()
                    .flex_col()
                    .gap(folio_measure.space(Space::Roomy))
                    .child(leaf.main)
                    .child(
                        div()
                            .flex()
                            .gap(folio_measure.space(Space::Base))
                            .child(
                                facet::icons::ui(facet::icons::Icon::Diamond, facet::icons::IconSize::S12, palette.ink4)
                                    .size(folio_measure.icon(10.0)),
                            )
                            .child(div().flex_1().min_w(px(0.0)).child(note)),
                    )
                    .into_any_element(),
                (None, _) => leaf.main,
            });
        }
        column
    }
}

/// One page the reader shows (or is still showing on its way out).
#[derive(Clone)]
struct Place {
    key: u64,
    route: Route,
    overlay: Option<Overlay>,
    way: Way,
    lens: Lens,
}

/// The reader's column geometry for one frame.
#[derive(Clone, Copy)]
struct Layout {
    folio: Pixels,
    beside: Pixels,
    content: Pixels,
    wide: bool,
    gutter: Pixels,
    margin: Pixels,
    measure: Measure,
    folio_measure: Measure,
}

/// How a page arrives, by the way the reader moved.
fn enter_act(way: Way, scale: f32) -> Act {
    let from = match way {
        Way::Down => Pose {
            y: 24.0 * scale,
            sx: 0.985,
            sy: 0.985,
            opacity: 0.0,
            ..Pose::REST
        },
        Way::Up => Pose {
            y: -24.0 * scale,
            sx: 1.015,
            sy: 1.015,
            opacity: 0.0,
            ..Pose::REST
        },
        Way::Across => Pose {
            x: 24.0 * scale,
            opacity: 0.0,
            ..Pose::REST
        },
        Way::View => Pose {
            opacity: 0.0,
            ..Pose::REST
        },
    };
    let duration = match way {
        Way::View => motion::QUICK,
        Way::Down | Way::Up | Way::Across => motion::EMPH,
    };
    Act {
        duration,
        pose: Keys::owned(duration, vec![(0.0, from), (1.0, Pose::REST)], motion::GLIDE),
        room: Keys::owned(duration, vec![(0.0, Extent::FULL), (1.0, Extent::FULL)], motion::GLIDE),
    }
}

/// How the page being replaced leaves: it fades as it steps back, quickly,
/// whichever way the reader moved (its exit is fixed when it arrives).
fn exit_act() -> Act {
    let to = Pose {
        sx: 0.99,
        sy: 0.99,
        opacity: 0.0,
        ..Pose::REST
    };
    Act {
        duration: motion::STD,
        pose: Keys::owned(motion::STD, vec![(0.0, Pose::REST), (1.0, to)], motion::DROP),
        room: Keys::owned(motion::STD, vec![(0.0, Extent::FULL), (1.0, Extent::FULL)], motion::GLIDE),
    }
}

impl Region for Reader {
    fn core(&mut self) -> &mut RegionCore {
        &mut self.core
    }

    fn keys(&self, snapshot: &AppSnapshot) -> Vec<PageKey> {
        reader_keys(snapshot)
    }

    fn observe(&mut self, event: &StoreEvent, store: &DataStore) {
        if event.is_branch(Branch::Route) || event.is_branch(Branch::Overlay) {
            let snapshot = store.snapshot();
            let overlay = snapshot.overlay().filter(|overlay| matches!(overlay, Overlay::Settings(_) | Overlay::Inbox));
            if *snapshot.route() != self.route || overlay != self.overlay {
                // (A release change or a view switch replaces the entry; it
                // still arrives as a new page.)
                self.arrive(snapshot.route(), overlay);
            }
        }
    }
}

/// The page keys the reader draws for a snapshot's place.
pub(crate) fn reader_keys(snapshot: &AppSnapshot) -> Vec<PageKey> {
    place_keys(snapshot.route(), snapshot.overlay())
}

impl Reader {
    /// Builds one place's body. The current place registers its targets and
    /// records its words; a leaving place is inert.
    fn body(
        &mut self,
        place: &Place,
        current: bool,
        snapshot: &AppSnapshot,
        layout: &Layout,
        facet: &facet::Facet,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let palette = facet.palette();
        let keys = place_keys(&place.route, place.overlay);
        let pages = Pages::gather(self.links.store.read(cx), &keys);
        let links = self.links.clone();
        let targets = if current { self.targets.clone() } else { Targets::default() };
        let mut said = Vec::new();
        let mut hero = Vec::new();
        let mut scratch_hover = HoverIntent::default();
        let mut hover = if current { std::mem::take(&mut self.hover) } else { HoverIntent::default() };
        let leaves = {
            let mut ctx = Ctx {
                measure: layout.folio_measure,
                note: if layout.wide { Measure::new(layout.margin, facet) } else { layout.folio_measure },
                palette,
                reveal: facet.reveal,
                links: &links,
                targets: &targets,
                lens: if current { self.lens } else { place.lens },
                said: &mut said,
                hero: &mut hero,
            };
            let hover = if current { &mut hover } else { &mut scratch_hover };
            bodies::build(&place.route, place.overlay, snapshot, &pages, &mut ctx, hover, cx)
        };
        if current {
            self.hover = hover;
            self.said = said;
            self.hero = hero;
        }
        Self::compose(leaves, layout, palette)
    }
}

/// The page keys a place draws.
fn place_keys(route: &Route, overlay: Option<Overlay>) -> Vec<PageKey> {
    match overlay {
        Some(Overlay::Settings(_)) => return vec![PageKey::Health],
        Some(Overlay::Inbox) => return Vec::new(),
        _ => {}
    }
    match route {
        Route::Orbit(_) => vec![PageKey::Orbit, PageKey::Health],
        Route::World => Vec::new(),
        Route::Package(_) => route_package(route).map(PageKey::Package).into_iter().collect(),
        Route::Symbol(symbol) => match symbol.view {
            View::Page | View::Graph => route_symbol(route).map(PageKey::Symbol).into_iter().collect(),
            View::Code => route_symbol(route)
                .map(|id| vec![PageKey::Source(id.clone()), PageKey::Symbol(id)])
                .unwrap_or_default(),
        },
    }
}

impl Render for Reader {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.core.rendered();
        self.targets.begin();
        let measure = self.core.measure(cx);
        let facet = cx.facet();
        let palette = facet.palette();
        let snapshot = self.links.snapshot(cx);
        let width = self.core.width();
        let pad = measure.fluid(22.0, 40.0);
        let content = (width - pad * 2.0).max(px(0.0));
        let scale = measure.scale();
        let notes_possible = matches!(snapshot.route(), Route::Symbol(route) if route.view == View::Code)
            && snapshot.overlay().is_none();
        let wide = measure.room() >= Room::Wide && notes_possible;
        let gutter = px(GUTTER * facet.density.space() * scale);
        let margin = px(MARGIN * scale);
        let beside = if wide { margin + gutter } else { px(0.0) };
        let folio = px(FOLIO * scale).min((content - beside).max(px(0.0)));
        let layout = Layout {
            folio,
            beside,
            content,
            wide,
            gutter,
            margin,
            measure,
            folio_measure: Measure::new(folio, &facet),
        };

        // The page stack: the current place enters, replaced places leave.
        let Some(current) = self.places.last().cloned() else {
            return div();
        };
        let items = self.pages.sync_entries(
            [Entry::new(("place", current.key)).enter(enter_act(current.way, scale)).exit(exit_act())],
            window,
            cx,
        );
        self.places.retain(|place| items.iter().any(|item| item.key == ElementId::from(("place", place.key))));
        let places = self.places.clone();
        let mut stack = div().relative().w_full().flex().justify_center();
        let mut leaving = Vec::new();
        for item in &items {
            let Some(place) = places.iter().find(|place| item.key == ElementId::from(("place", place.key))) else {
                continue;
            };
            let is_current = place.key == current.key;
            let body = self.body(place, is_current, &snapshot, &layout, &facet, cx);
            let slot = item.slot(div().id(item.key.clone()).child(body));
            if is_current {
                stack = stack.child(slot);
            } else {
                leaving.push(slot);
            }
        }
        // Leaving pages paint over the arriving one, inert, and take no room.
        for slot in leaving {
            stack = stack.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(slot)
                    .occlude(),
            );
        }

        let glow = self.targets.glow(&self.motion, &measure);
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("reader-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .w_full()
                            .px(pad)
                            .pt(measure.fluid(22.0, 56.0))
                            .pb(px(96.0 * scale))
                            .child(stack),
                    ),
            )
            .child(glow)
            .text_color(palette.ink1.hsla())
            .font_family(facet::fonts::family(ty::BODY))
    }
}
