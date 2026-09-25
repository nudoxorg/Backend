//! The windowed float storm: a real headless window and renderer driven by
//! the harness with seeded input — pointer moves over triggers, cards and
//! empty space, clicks, scrolls, Esc / Space / keyboard opens, triggers
//! unmounting under open cards, resizes, waits, and bursts of direct layer
//! calls inside a single frame. After every step it checks the invariants;
//! after a neutral tail it checks that nothing asks for frames and that the
//! pixels equal a fresh boot straight into the same final state.

#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use super::{FloatKind, FloatRequest, Side, state};
use crate::gallery;
use crate::Typeset;
use crate::measure::Set;
use crate::motion;
use crate::theme::{ActiveFacet, Facet};
use crate::tokens::ty;
use backend_gui_harness::{
    AnimationFrame, CaptureError, GpuiCaptureOptions, GuiState, InputStep, Viewport,
    capture_gpui_state_with_adapters_result_and_semantics,
};
use gpui::{
    AnyElement, App, AppContext, Bounds, Context, ElementId, FocusHandle, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Pixels, Render, SharedString, Styled, Window, div,
    point, px, size,
};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::{Mutex, PoisonError};

/// Headless captures own process-global native state: one at a time.
static PLATFORM: Mutex<()> = Mutex::new(());

const WIDTH: u32 = 1100;
const HEIGHT: u32 = 720;
const TRIGGERS: usize = 12;

fn trigger_rect(index: usize) -> Bounds<Pixels> {
    let (col, row) = (index % 4, index / 4);
    Bounds::new(
        point(px(40.0 + col as f32 * 230.0), px(60.0 + row as f32 * 200.0)),
        size(px(120.0), px(20.0)),
    )
}

fn kind_of(path: &str) -> FloatKind {
    let root: usize = path
        .trim_start_matches('p')
        .split('/')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let nested = path.contains('/');
    match root {
        _ if nested => FloatKind::Peek,
        8 | 9 => FloatKind::Lens,
        10 => FloatKind::Menu,
        11 => FloatKind::Tip,
        _ => FloatKind::Peek,
    }
}

fn key_of(path: &str) -> ElementId {
    ElementId::Name(SharedString::from(format!("storm:{path}")))
}

fn path_of(key: &ElementId) -> Option<String> {
    match key {
        ElementId::Name(name) => name.strip_prefix("storm:").map(str::to_owned),
        _ => None,
    }
}

fn request(path: &str, anchor: Bounds<Pixels>) -> FloatRequest {
    let side = if path.contains('/') { Side::Right } else { Side::Below };
    let owned = path.to_owned();
    FloatRequest::new(key_of(path), anchor, kind_of(path), move |measure, window, cx| {
        content(&owned, measure, window, cx)
    })
    .side(side)
}

/// A card: its path as a title, and two rest-able words that chain.
fn content(path: &str, measure: &crate::Measure, window: &mut Window, cx: &mut App) -> AnyElement {
    super::title(SharedString::from(path.to_owned()), window, cx);
    let palette = cx.facet().palette();
    // Like a real peek, a pinned row is its name only: no rest-able words.
    if super::surface(window, cx) == super::Surface::Pinned {
        return div()
            .set(ty::MONO_ROW, measure)
            .text_color(palette.ink1.hsla())
            .child(SharedString::from(path.to_owned()))
            .into_any_element();
    }
    let words = ["alpha", "beta"].into_iter().map(|word| {
        let child = format!("{path}/{word}");
        super::trigger(
            key_of(&child),
            move |bounds| request(&child, bounds),
            div().px(px(4.0)).child(word),
        )
    });
    div()
        .w(px(260.0).min(measure.width()))
        .p(px(12.0))
        .flex()
        .flex_col()
        .gap(px(12.0))
        .set(ty::MONO_ROW, measure)
        .text_color(palette.ink1.hsla())
        .child(SharedString::from(path.to_owned()))
        .child(div().flex().gap(px(16.0)).children(words))
        .into_any_element()
}

/// What the stormed run hands to its checks and to the fresh run.
#[derive(Default)]
struct Shared {
    focus: Option<FocusHandle>,
    removed: BTreeSet<usize>,
    toggles: usize,
    coverage: Coverage,
}

/// How much of the layer's state space a storm visited.
#[derive(Default, Debug)]
struct Coverage {
    steps: usize,
    any_card: usize,
    depth2: usize,
    depth3: usize,
    overflow: usize,
    tip: usize,
    leaving: usize,
    focus_in_card: usize,
    pins_max: usize,
}

struct Board {
    focus: FocusHandle,
    shared: Rc<RefCell<Shared>>,
}

impl Render for Board {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let removed = self.shared.borrow().removed.clone();
        let mut root = div()
            .relative()
            .size_full()
            .bg(palette.g1.hsla())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "o" => open_under_pointer(window, cx),
                    "t" => {
                        let mut shared = this.shared.borrow_mut();
                        let index = (shared.toggles * 5) % TRIGGERS;
                        shared.toggles += 1;
                        if !shared.removed.remove(&index) {
                            shared.removed.insert(index);
                        }
                    }
                    _ => {
                        super::handle_key(&event.keystroke, window, cx);
                    }
                }
                cx.notify();
            }))
            .typeset(ty::MONO_ROW, &facet);
        for index in (0..TRIGGERS).filter(|index| !removed.contains(index)) {
            let rect = trigger_rect(index);
            let path = format!("p{index}");
            root = root.child(
                div()
                    .absolute()
                    .left(rect.origin.x)
                    .top(rect.origin.y)
                    .w(rect.size.width)
                    .h(rect.size.height)
                    .child(super::trigger(
                        key_of(&path),
                        move |bounds| request(&path, bounds),
                        div()
                            .size_full()
                            .text_color(palette.ink0.hsla())
                            .child(format!("trigger {index}")),
                    )),
            );
        }
        let column = facet.measure(px(252.0));
        root.child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .w(px(280.0))
                .p(px(14.0))
                .border_l_1()
                .border_color(palette.line1.hsla())
                .child(super::pinned_column(&column, window, cx)),
        )
        .child(super::layer(window, cx))
    }
}

/// "o": opens (keyboard) whatever tracked trigger the pointer is on.
fn open_under_pointer(window: &mut Window, cx: &mut App) {
    let at = window.mouse_position();
    let hit = {
        let layer = state(window, cx);
        let layer = layer.borrow();
        layer
            .triggers
            .iter()
            .filter(|(_, report)| report.hovered && report.bounds.contains(&at))
            .filter_map(|(key, report)| path_of(key).map(|path| (path, report.bounds)))
            .max_by_key(|(path, _)| path.len())
    };
    if let Some((path, bounds)) = hit {
        super::open(request(&path, bounds), window, cx);
    }
}

// ------------------------------------------------------------------ input

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Where cards tend to be: under a trigger (a root) and right of one (a
/// child), with their words about 40 px down.
fn likely_card_point(rng: &mut Rng) -> (f32, f32) {
    let rect = trigger_rect(rng.below(TRIGGERS as u64) as usize);
    let (x, y) = (f32::from(rect.origin.x), f32::from(rect.origin.y));
    let depth = rng.below(3) as f32;
    let word = if rng.below(2) == 0 { 20.0 } else { 80.0 };
    (
        x - 6.0 + depth * 276.0 + word + rng.unit() * 20.0,
        y + 26.0 + 44.0 + depth * 40.0 + rng.unit() * 10.0,
    )
}

fn storm_steps(seed: u64, count: usize) -> Vec<InputStep> {
    let mut rng = Rng(seed);
    let mut steps = Vec::with_capacity(count + 16);
    let key = |value: &str| InputStep::Key {
        value: value.to_owned(),
    };
    for _ in 0..count {
        let step = match rng.below(100) {
            0..=29 => {
                let rect = trigger_rect(rng.below(TRIGGERS as u64) as usize);
                InputStep::PointerMove {
                    x: f32::from(rect.origin.x) + 10.0 + rng.unit() * 100.0,
                    y: f32::from(rect.origin.y) + 4.0 + rng.unit() * 12.0,
                    pressed_button: None,
                }
            }
            30..=49 => {
                let (x, y) = likely_card_point(&mut rng);
                InputStep::PointerMove {
                    x,
                    y,
                    pressed_button: None,
                }
            }
            50..=59 => InputStep::PointerMove {
                x: rng.unit() * WIDTH as f32,
                y: rng.unit() * HEIGHT as f32,
                pressed_button: None,
            },
            60..=63 => InputStep::Click {
                x: rng.unit() * WIDTH as f32,
                y: rng.unit() * HEIGHT as f32,
                button: "left".to_owned(),
            },
            64..=66 => InputStep::Scroll {
                x: rng.unit() * WIDTH as f32,
                y: rng.unit() * HEIGHT as f32,
                delta_x: 0.0,
                delta_y: rng.unit() * 40.0 - 20.0,
            },
            67..=70 => key("escape"),
            71..=74 => key("space"),
            75..=81 => key("o"),
            82..=83 => key("t"),
            84..=85 => InputStep::Resize {
                width: 760 + rng.below(560) as u32,
                height: 520 + rng.below(300) as u32,
            },
            86..=89 => key("b"),
            90..=95 => {
                // Dive into the deepest card, then give the rest time to rise.
                steps.push(key("d"));
                InputStep::Wait {
                    milliseconds: 380 + rng.below(80),
                }
            }
            _ => InputStep::Wait {
                milliseconds: rng.below(160),
            },
        };
        steps.push(step);
    }
    // The neutral tail: back to the start size, the pointer in empty space,
    // every transient dismissed, time for every exit to settle.
    steps.push(InputStep::Resize {
        width: WIDTH,
        height: HEIGHT,
    });
    steps.push(InputStep::PointerMove {
        x: 600.0,
        y: 690.0,
        pressed_button: None,
    });
    for _ in 0..4 {
        steps.push(key("escape"));
    }
    steps.push(InputStep::Wait { milliseconds: 1_500 });
    steps
}

/// "d": moves the pointer (a real event) onto a word of the deepest open
/// card, or opens that word from the keyboard.
fn dive(rng: &mut Rng, window: &mut Window, cx: &mut App) {
    let word = {
        let layer = state(window, cx);
        let layer = layer.borrow();
        let Some(top) = layer.model.top() else {
            return;
        };
        let Some(parent) = path_of(&top.key) else {
            return;
        };
        let pick = if rng.below(2) == 0 { "alpha" } else { "beta" };
        let path = format!("{parent}/{pick}");
        // Only a word that is on the card as it is drawn now (a report from
        // an earlier incarnation of the card is not a visible word).
        layer
            .triggers
            .get(&key_of(&path))
            .filter(|report| top.covers(report.bounds.center()))
            .map(|report| (path, report.bounds))
    };
    let Some((path, bounds)) = word else { return };
    if rng.below(3) == 0 {
        super::open(request(&path, bounds), window, cx);
    } else {
        let event = gpui::MouseMoveEvent {
            position: bounds.center(),
            pressed_button: None,
            modifiers: gpui::Modifiers::default(),
        };
        window.dispatch_event(gpui::PlatformInput::MouseMove(event), cx);
    }
}

/// A burst of direct layer calls inside one frame (no draw in between).
fn burst(rng: &mut Rng, removed: &BTreeSet<usize>, window: &mut Window, cx: &mut App) {
    for _ in 0..(3 + rng.below(12)) {
        let index = rng.below(TRIGGERS as u64) as usize;
        // Only mounted triggers report.
        if removed.contains(&index) {
            continue;
        }
        let root = format!("p{index}");
        // A word of a card reports from inside that card, as a real word
        // does; when the card is not open, the word does not exist.
        let (path, anchor) = if rng.below(3) == 0 {
            let plate = {
                let layer = state(window, cx);
                let layer = layer.borrow();
                layer
                    .model
                    .cards()
                    .find(|card| card.is_open() && card.key == key_of(&root))
                    .and_then(|card| card.painted)
            };
            let Some(plate) = plate else { continue };
            let word = Bounds::new(
                point(plate.origin.x + px(16.0), plate.origin.y + px(40.0)),
                size(px(40.0), px(18.0)),
            );
            (format!("{root}/alpha"), word)
        } else {
            (root, trigger_rect(index))
        };
        match rng.below(8) {
            0..=2 => super::rest(request(&path, anchor), window, cx),
            3 => super::leave(&key_of(&path), window, cx),
            4 => {
                if std::env::var("FLOAT_TRACE").is_ok() {
                    eprintln!("  burst open {path}");
                }
                super::open(request(&path, anchor), window, cx);
            }
            5 => {
                super::step_back(window, cx);
            }
            6 => {
                super::pin_top(window, cx);
            }
            _ => {
                let pins = super::pins(window, cx);
                if let Some(key) = pins.get(rng.below(pins.len().max(1) as u64) as usize) {
                    super::unpin(key, window, cx);
                }
            }
        }
    }
}

// ------------------------------------------------------------------ checks

/// Every invariant, checked after every step.
fn check(step: usize, removed_before: &BTreeSet<usize>, shared: &Shared, window: &mut Window, cx: &mut App) {
    let layer = state(window, cx);
    let layer = layer.borrow();
    let now = motion::now(cx);
    let chain = layer.model.chain();
    assert!(chain.len() <= super::MAX_DEPTH, "step {step}: chain of {}", chain.len());
    for (index, card) in chain.iter().enumerate() {
        assert_eq!(card.level, Some(index), "step {step}: chain levels not contiguous");
    }
    let open_tips = layer
        .model
        .cards()
        .filter(|card| card.is_open() && card.level.is_none())
        .count();
    assert!(open_tips <= 1, "step {step}: {open_tips} tips open");
    let open: BTreeSet<String> = layer
        .model
        .cards()
        .filter(|card| card.is_open())
        .filter_map(|card| path_of(&card.key))
        .collect();
    for card in layer.model.cards() {
        let value = card.presence.value(now);
        assert!((0.0..=1.0).contains(&value), "step {step}: presence {value}");
        if !card.is_open() {
            continue;
        }
        let Some(path) = path_of(&card.key) else { continue };
        // A card whose trigger unmounted at least one frame ago is gone.
        let root: usize = path
            .trim_start_matches('p')
            .split('/')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(usize::MAX);
        let root_alive = !removed_before.contains(&root) || !shared.removed.contains(&root);
        if !(root_alive || path.contains('/')) {
            let cards: Vec<String> = layer
                .model
                .cards()
                .map(|card| format!("{}@{:?}{}{}", card.key, card.level, if card.is_open() { "" } else { " (leaving)" }, if card.sticky { " sticky" } else { "" }))
                .collect();
            let report = layer.triggers.get(&card.key).map(|r| (r.frame, r.seq, r.view.is_some()));
            panic!(
                "step {step}: card {path} outlived its trigger; cards {cards:?}; report {report:?}; layer frame {} seq {}",
                layer.frame, layer.seq
            );
        }
        // A word's card lives only while the card holding the word is open.
        if let Some((parent, _)) = path.rsplit_once('/') {
            let chain: Vec<String> = layer
                .model
                .cards()
                .map(|card| format!("{}@{:?}{}{}", card.key, card.level, if card.is_open() { "" } else { " (leaving)" }, if card.sticky { " sticky" } else { "" }))
                .collect();
            assert!(
                open.contains(parent),
                "step {step}: card {path} is open but its parent {parent} is not; cards {chain:?}"
            );
        }
    }
    let _ = &shared.coverage;
    // Focus sits on a live element: nothing, the board, or an open card.
    if let Some(focused) = window.focused(cx) {
        let on_board = shared.focus.as_ref() == Some(&focused);
        let on_open_card = layer.focus.iter().any(|(id, card)| {
            card.handle == focused && layer.model.card(*id).is_some_and(super::Card::is_open)
        });
        if !(on_board || on_open_card) {
            let entries: Vec<String> = layer
                .focus
                .iter()
                .map(|(id, card)| {
                    let found = layer.model.card(*id);
                    format!(
                        "#{id} {:?} open={} focused={}",
                        found.map(|card| card.key.to_string()),
                        found.is_some_and(super::Card::is_open),
                        card.handle == focused
                    )
                })
                .collect();
            let cards: Vec<String> = layer
                .model
                .cards()
                .map(|card| format!("#{} {}@{:?}{}", card.id, card.key, card.level, if card.is_open() { "" } else { " (leaving)" }))
                .collect();
            panic!("step {step}: focus on a dead element; focus entries {entries:?}; cards {cards:?}");
        }
    }
}

type Result<T> = std::result::Result<T, CaptureError>;

/// Runs one capture of the board with `steps`, returns the final two frames
/// (at `end` and `end + 400`) and the frames requested at each.
fn run(
    id: &str,
    steps: &[InputStep],
    end: u64,
    seed: u64,
    prepare: impl FnOnce(&mut Window, &mut App) + 'static,
    shared: Rc<RefCell<Shared>>,
) -> Result<(Vec<image::RgbaImage>, Vec<u64>, Vec<String>)> {
    let viewport = Viewport::new(WIDTH, HEIGHT, 1)?;
    let frames = [
        AnimationFrame {
            label: "end".into(),
            time_ms: end,
        },
        AnimationFrame {
            label: "later".into(),
            time_ms: end + 400,
        },
    ];
    let requested = Rc::new(RefCell::new(Vec::new()));
    let pins_out = Rc::new(RefCell::new(Vec::new()));
    let mut rng = Rng(seed ^ 0xB0B5);
    let mut step_index = 0usize;
    let mut removed_before = BTreeSet::new();
    let prepare = RefCell::new(Some(prepare));
    let set = capture_gpui_state_with_adapters_result_and_semantics(
        viewport,
        GuiState::new(id, None, None),
        steps,
        &frames,
        GpuiCaptureOptions {
            asset_source: std::sync::Arc::new(crate::icons::Assets),
            ..GpuiCaptureOptions::default()
        },
        {
            let requested = Rc::clone(&requested);
            let pins_out = Rc::clone(&pins_out);
            move |_frame, window, cx| {
                requested.borrow_mut().push(motion::frames_requested(cx));
                *pins_out.borrow_mut() = super::pins(window, cx)
                    .iter()
                    .filter_map(path_of)
                    .collect::<Vec<String>>();
                Ok(())
            }
        },
        {
            let shared = Rc::clone(&shared);
            move |step, window, cx| {
                if let Some(prepare) = prepare.borrow_mut().take() {
                    prepare(window, cx);
                }
                if matches!(step, InputStep::Key { value } if value == "d") {
                    dive(&mut rng, window, cx);
                }
                if matches!(step, InputStep::Key { value } if value == "b") {
                    let removed = shared.borrow().removed.clone();
                    burst(&mut rng, &removed, window, cx);
                }
                step_index += 1;
                if std::env::var("FLOAT_STORM_TRACE").is_ok() {
                    eprintln!("step {step_index}: {step:?}");
                }
                let now_removed = shared.borrow().removed.clone();
                check(step_index, &removed_before, &shared.borrow(), window, cx);
                removed_before = now_removed;
                let layer = state(window, cx);
                let layer = layer.borrow();
                let chain = layer.model.chain();
                let focused = window.focused(cx);
                let mut shared = shared.borrow_mut();
                let coverage = &mut shared.coverage;
                coverage.steps += 1;
                coverage.any_card += usize::from(!chain.is_empty());
                coverage.depth2 += usize::from(chain.len() >= 2);
                coverage.depth3 += usize::from(chain.len() >= 3);
                coverage.overflow += usize::from(chain.iter().any(|card| card.overflow));
                coverage.tip += usize::from(layer.model.tip().is_some());
                coverage.leaving += usize::from(layer.model.cards().any(|card| !card.is_open()));
                coverage.focus_in_card += usize::from(
                    focused.is_some_and(|focused| layer.focus.values().any(|card| card.handle == focused)),
                );
                coverage.pins_max = coverage.pins_max.max(layer.model.pins().len());
            }
        },
        |_frame, _image, _viewport, _window, _cx| Ok(None),
        {
            let shared = Rc::clone(&shared);
            move |window, cx| {
                gallery::bootstrap(Facet::default(), false, cx).expect("bootstrap");
                let focus = cx.focus_handle();
                shared.borrow_mut().focus = Some(focus.clone());
                window.focus(&focus, cx);
                cx.new(|_| Board { focus, shared })
            }
        },
    )?;
    let images = set.frames.into_iter().map(|frame| frame.image).collect();
    let requested = requested.borrow().clone();
    let pins = pins_out.borrow().clone();
    Ok((images, requested, pins))
}

fn storm(seed: u64, count: usize) {
    let _platform = PLATFORM.lock().unwrap_or_else(PoisonError::into_inner);
    let steps = storm_steps(seed, count);
    let waited: u64 = steps
        .iter()
        .map(|step| match step {
            InputStep::Wait { milliseconds } => *milliseconds,
            _ => 0,
        })
        .sum();
    let end = waited + 100;
    let shared = Rc::new(RefCell::new(Shared::default()));
    let (stormed, requested, pins) = run(
        "float-storm",
        &steps,
        end,
        seed,
        |_, _| {},
        Rc::clone(&shared),
    )
    .expect("stormed capture");
    // Nothing moves after the tail: the two final frames match and no frame
    // was requested between them.
    assert_eq!(stormed.len(), 2);
    assert!(
        stormed[0].as_raw() == stormed[1].as_raw(),
        "seed {seed}: the settled frame still changes 400 ms later"
    );
    assert_eq!(
        requested[0], requested[1],
        "seed {seed}: frames were requested after the storm settled ({} -> {})",
        requested[0], requested[1]
    );
    // The fresh boot: same unmounted triggers, the same pins in the same
    // order, the pointer in the same place; nothing stormed.
    let removed = shared.borrow().removed.clone();
    eprintln!(
        "storm seed {seed}: {} steps, pins at the end {pins:?}, unmounted triggers {removed:?}\n  coverage {:?}",
        steps.len(),
        shared.borrow().coverage
    );
    let fresh_shared = Rc::new(RefCell::new(Shared {
        removed,
        ..Shared::default()
    }));
    let fresh_steps = vec![
        InputStep::PointerMove {
            x: 600.0,
            y: 690.0,
            pressed_button: None,
        },
        InputStep::Wait { milliseconds: 10 },
    ];
    let (fresh, _, _) = run(
        "float-fresh",
        &fresh_steps,
        end,
        seed,
        move |window, cx| {
            for path in pins.iter().rev() {
                let anchor = Bounds::new(point(px(-50.0), px(-50.0)), size(px(1.0), px(1.0)));
                super::open(request(path, anchor), window, cx);
                super::pin_top(window, cx);
            }
            super::settle_now(window, cx);
        },
        fresh_shared,
    )
    .expect("fresh capture");
    if let Ok(dir) = std::env::var("FLOAT_STORM_OUT") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let _ = stormed[0].save(dir.join(format!("storm-{seed}-settled.png")));
        let _ = fresh[0].save(dir.join(format!("storm-{seed}-fresh.png")));
    }
    if stormed[0].as_raw() != fresh[0].as_raw() {
        let dir = std::env::temp_dir().join("facet-float-storm");
        let _ = std::fs::create_dir_all(&dir);
        let _ = stormed[0].save(dir.join(format!("stormed-{seed}.png")));
        let _ = fresh[0].save(dir.join(format!("fresh-{seed}.png")));
        panic!("seed {seed}: settled storm differs from a fresh boot (see {})", dir.display());
    }
}

#[test]
fn float_storm_keeps_every_invariant_and_settles_to_a_fresh_boot() {
    let seeds: Vec<u64> = std::env::var("FLOAT_STORM_SEEDS")
        .ok()
        .map(|list| list.split(',').filter_map(|s| s.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![7, 1_234]);
    let count: usize = std::env::var("FLOAT_STORM_STEPS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1_500);
    for seed in seeds {
        storm(seed, count);
    }
}
