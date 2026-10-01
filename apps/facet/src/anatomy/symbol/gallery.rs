//! The simple symbol page in the gallery, built from the design board's own
//! page models (`Nudox-Design-System/v6/moments/data/page6/<id>.json`): the
//! same seven pages, so a capture sits beside the board's still. The facts
//! are read from the board's raw fields (declaration, docs, members, use
//! sites); everything the page says is derived from them by
//! [`derive`](super::derive), never copied from the board's own derivation.
//!
//! State is the gallery's own: a click on a chip, a fold or a package changes
//! it, so a script can open any state before a capture.

use super::board::{board, board_dir};
use super::host::{Act, Change, Host, Spots, Ui};
use super::key::{FoldKey, Key, PortName, Sec, Slot};
use super::kit::{Env, ink, recorded, recorded_after, said};
use super::layout::Layout;
use super::view::{Uses, View};
use super::{Chrome, page};
use crate::anatomy::page::{Door, Doors, Fold, Still};
use crate::gallery::Scene;
use crate::motion::Flow;
use crate::motion::presence::Presence;
use crate::overlay::float;
use crate::theme::ActiveFacet;
use gpui::{AnyElement, AnyView, App, AppContext, Context, EntityId, InteractiveElement, IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, px};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

const READER: (u32, u32) = (1176, 1500);

pub(crate) const SCENES: &[Scene] = &[
    Scene { id: "sym6-from-str", title: "serde_json::from_str: a generic that can fail (board: symbol6 rs-from_str)", size: READER, build: |_, cx| scene("rs-from_str", &[], cx) },
    Scene { id: "sym6-value", title: "serde_json::Value: an enum that nests (board: symbol6 rs-Value)", size: READER, build: |_, cx| scene("rs-Value", &[], cx) },
    Scene { id: "sym6-as-str", title: "Value::as_str: a method that may give nothing (board: symbol6 rs-as_str)", size: READER, build: |_, cx| scene("rs-as_str", &[], cx) },
    Scene { id: "sym6-alloc", title: "AllocationInfo: a struct with public fields (board: symbol6 rs-AllocationInfo)", size: READER, build: |_, cx| scene("rs-AllocationInfo", &[], cx) },
    Scene { id: "sym6-serialize", title: "serde::Serialize: a trait (board: symbol6 rs-Serialize)", size: READER, build: |_, cx| scene("rs-Serialize", &[], cx) },
    Scene { id: "sym6-py-match", title: "re.match: Python with no type hints (board: symbol6 py-re.match)", size: READER, build: |_, cx| scene("py-re.match", &[], cx) },
    Scene { id: "sym6-js-which", title: "which.sync: JavaScript, an options object, unfolded (board: symbol6 js-which.sync &opts=opt)", size: READER, build: |_, cx| scene("js-which.sync", &[FoldKey::Options(PortName::new("opt"))], cx) },
];

// ------------------------------------------------------------------ the scene

struct State {
    ui: Ui,
    open: BTreeSet<FoldKey>,
    presences: BTreeMap<FoldKey, Presence>,
    spots: Rc<Spots>,
    flow: Flow,
}

struct GalleryHost {
    state: Rc<RefCell<State>>,
    view: EntityId,
    doors: Still,
}

impl Doors for GalleryHost {
    fn door(&self, _: &str) -> Option<Door> {
        None
    }
    fn fold(&self, _: &'static str) -> Option<Fold> {
        None
    }
    fn track(&self, key: SharedString, label: SharedString, door: Option<&Door>, element: AnyElement) -> AnyElement {
        self.doors.track(key, label, door, element)
    }
    fn say(&self, _: &str) {}
}

impl Host for GalleryHost {
    fn ui(&self) -> Ui {
        self.state.borrow().ui.clone()
    }

    fn change(&self, change: Change) -> Act {
        let (state, view) = (Rc::clone(&self.state), self.view);
        Rc::new(move |_, cx| {
            let next = state.borrow().ui.clone().apply(&change);
            state.borrow_mut().ui = next;
            cx.notify(view);
        })
    }

    fn open_source(&self, _: &str, _: u32) -> Act {
        Rc::new(|_, _| {})
    }

    fn unfold(&self, key: &FoldKey) -> Option<Fold> {
        let mut state = self.state.borrow_mut();
        let presence = state.presences.entry(key.clone()).or_insert_with(|| Presence::new(format!("sym6-gallery-{key:?}"))).clone();
        let open = state.open.contains(key);
        drop(state);
        let (state, view, key) = (Rc::clone(&self.state), self.view, key.clone());
        Some(Fold {
            open,
            presence,
            toggle: Rc::new(move |_, cx| {
                let mut state = state.borrow_mut();
                if !state.open.remove(&key) {
                    state.open.insert(key.clone());
                }
                drop(state);
                cx.notify(view);
            }),
        })
    }

    fn lookup(&self, _: &str) -> Option<Act> {
        None
    }

    fn target(&self, key: &Key, _: SharedString, _: Act, element: AnyElement) -> AnyElement {
        // Published like the shell's walk does, so lint sees every stop and
        // holds it to the 24 px rule.
        crate::probe::target(key.id(), crate::probe::Target { clickable: true, focusable: true, ..crate::probe::Target::default() }, element).into_any_element()
    }

    fn reveal(&self, _: Sec) -> Act {
        Rc::new(|_, _| {})
    }

    fn spots(&self) -> Rc<Spots> {
        Rc::clone(&self.state.borrow().spots)
    }

    fn flow(&self) -> Flow {
        self.state.borrow().flow.clone()
    }
}

struct SymbolScene {
    view: View,
    uses: Uses,
    state: Rc<RefCell<State>>,
}

fn scene(id: &str, open: &[FoldKey], cx: &mut App) -> AnyView {
    let Some((view, uses)) = board(id) else {
        return cx.new(|_: &mut Context<Missing>| Missing(id.to_owned())).into();
    };
    let state = Rc::new(RefCell::new(State { ui: Ui::default(), open: open.iter().cloned().collect(), presences: BTreeMap::new(), spots: Spots::new(), flow: Flow::new("sym6-gallery") }));
    cx.new(|_: &mut Context<SymbolScene>| SymbolScene { view, uses, state }).into()
}

struct Missing(String);

impl Render for Missing {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.facet().palette();
        div().size_full().bg(palette.g1.hsla()).child(SharedString::from(format!("the board's page model `{}` is not at {}", self.0, board_dir().display())))
    }
}

impl Render for SymbolScene {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let window_room = crate::fluid::Room::new(window.viewport_size().width, facet.text_scale);
        // The reader's own gutters and top space, as fluid tokens.
        let pad = crate::tokens::fluid::READER_PAD.at(window_room);
        let top = crate::tokens::fluid::READER_TOP.at(window_room);
        let measure = facet.measure((window.viewport_size().width - pad * 2.0).max(px(0.0)));
        let modes = crate::fluid::Modes::keyed(gpui::ElementId::Name("sym6-modes".into()), window, cx);
        let host = GalleryHost { state: Rc::clone(&self.state), view: cx.entity_id(), doors: Still };
        let lay = Layout::of(&measure, &modes);
        let env = Env { m: measure.within(lay.main), p: palette, host: &host, lay };
        let i = ink(palette);
        let title = div().flex().items_baseline().self_start().child(said(&env, &Key::of(super::key::Part::Kind).field(Slot::Title), self.view.head.name.clone(), crate::tokens::scale::DISPLAY, i.ink0)).into_any_element();
        let gem = super::gem(&self.view, &measure, palette);
        let element = page(&self.view, &self.uses, Chrome { gem, title }, &measure, palette, &host, &modes);
        // The scene's scroll container tells the probe how far it scrolls, so lint knows the page below the fold is reachable.
        let content = Rc::new(std::cell::Cell::new(gpui::Bounds::<gpui::Pixels>::default()));
        let record_content = Rc::clone(&content);
        let inner = recorded(move |bounds, _| record_content.set(bounds), div().px(pad).pt(top).pb(px(120.0)).child(element).into_any_element());
        let scroll = recorded_after(
            move |viewport, cx| crate::probe::record_scroll(cx, &gpui::ElementId::Name("sym6-scroll".into()), viewport, content.get()),
            div().id("sym6-scroll").size_full().overflow_y_scroll().child(inner).into_any_element(),
        );
        div().size_full().relative().overflow_hidden().bg(palette.g1.hsla()).child(scroll).child(float::layer(window, cx))
    }
}
