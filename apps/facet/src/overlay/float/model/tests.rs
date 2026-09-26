//! Model tests: hover intent, warm sweep, aim, chain, exits, pins, and a
//! seeded storm over the pure model. (The windowed storm — frames, focus,
//! pixels — lives in `overlay::float::storm`.)

use super::{AIM_IDLE, MAX_DEPTH, Model, WARM};
use crate::overlay::float::{FloatKind, FloatRequest};
use gpui::{Bounds, ElementId, IntoElement, ParentElement, Pixels, Point, div, point, px, size};
use std::collections::HashSet;
use std::time::{Duration, Instant};

fn b(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
}

fn at(x: f32, y: f32) -> Point<Pixels> {
    point(px(x), px(y))
}

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

fn req(key: &str, anchor: Bounds<Pixels>, kind: FloatKind) -> FloatRequest {
    let label: gpui::SharedString = key.to_owned().into();
    FloatRequest::new(ElementId::Name(label.clone()), anchor, kind, move |_, _, _| {
        div().child(label.clone()).into_any_element()
    })
}

fn key(name: &str) -> ElementId {
    ElementId::Name(name.to_owned().into())
}

/// A card is painted where the layer would put it: below its anchor.
fn paint_below(model: &mut Model, name: &str, width: f32, height: f32) -> Bounds<Pixels> {
    let card = model
        .cards()
        .find(|card| card.is_open() && card.key == key(name))
        .map(|card| (card.id, card.anchor))
        .expect("open card");
    let plate = b(
        f32::from(card.1.origin.x) - 6.0,
        f32::from(card.1.origin.y + card.1.size.height) + 6.0,
        width,
        height,
    );
    model.painted(card.0, plate);
    plate
}

fn open_keys(model: &Model) -> Vec<String> {
    model.chain().iter().map(|card| card.key.to_string()).collect()
}

#[test]
fn a_cold_rest_rises_after_the_rest_delay_and_not_before() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.rest(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.tick(t0 + ms(349));
    assert!(model.chain().is_empty(), "rose before 350 ms");
    model.tick(t0 + ms(350));
    assert_eq!(open_keys(&model), ["a"]);
}

#[test]
fn leaving_before_the_delay_cancels_the_rest() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.rest(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.leave(&key("a"), t0 + ms(200));
    model.tick(t0 + ms(1_000));
    assert!(model.chain().is_empty());
    assert!(model.is_settled(t0 + ms(1_000)));
}

#[test]
fn a_rest_repeated_on_the_same_trigger_does_not_restart_the_delay() {
    let t0 = Instant::now();
    let mut model = Model::new();
    let anchor = b(100.0, 100.0, 60.0, 18.0);
    model.rest(req("a", anchor, FloatKind::Peek), t0);
    model.rest(req("a", anchor, FloatKind::Peek), t0 + ms(300));
    model.tick(t0 + ms(350));
    assert_eq!(open_keys(&model), ["a"], "the second report must not push the rise back");
}

#[test]
fn warm_sweep_morphs_the_same_card_to_the_next_trigger_at_once() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.rest(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.tick(t0 + ms(350));
    let first = model.top().expect("open").id;
    let t1 = t0 + ms(600);
    model.leave(&key("a"), t1);
    model.rest(req("b", b(200.0, 100.0, 60.0, 18.0), FloatKind::Peek), t1);
    let top = model.top().expect("b opens without a delay");
    assert_eq!(top.key, key("b"));
    assert_eq!(top.id, first, "the card morphs; it is not closed and reopened");
    assert!(top.swapped.is_some());
    assert_eq!(model.cards().count(), 1, "no second card was created");
}

#[test]
fn a_kind_stays_warm_for_300_ms_after_it_closed_then_goes_cold() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.rest(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.tick(t0 + ms(350));
    model.leave(&key("a"), t0 + ms(400));
    model.pointer_at(Some(at(10.0, 10.0)), t0 + ms(400));
    model.tick(t0 + ms(400) + FloatKind::Peek.grace());
    assert!(model.chain().is_empty(), "closed after the grace");
    let closed_at = t0 + ms(400) + FloatKind::Peek.grace();
    // Within the warm window: immediate.
    model.rest(req("b", b(200.0, 100.0, 60.0, 18.0), FloatKind::Peek), closed_at + ms(250));
    assert_eq!(open_keys(&model), ["b"]);
    // Close it and wait past the warm window and the exit: cold again.
    model.close_all(closed_at + ms(260));
    let later = closed_at + ms(260) + WARM + ms(200);
    model.tick(later);
    model.rest(req("c", b(300.0, 100.0, 60.0, 18.0), FloatKind::Peek), later);
    assert!(model.chain().is_empty(), "cold after the warm window");
    model.tick(later + ms(350));
    assert_eq!(open_keys(&model), ["c"]);
}

#[test]
fn moving_into_the_card_keeps_it_and_moving_on_dismisses_it() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.rest(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.tick(t0 + ms(350));
    let plate = paint_below(&mut model, "a", 392.0, 240.0);
    let t1 = t0 + ms(500);
    model.leave(&key("a"), t1);
    model.pointer_at(Some(plate.center()), t1 + ms(20));
    model.tick(t1 + ms(2_000));
    assert_eq!(open_keys(&model), ["a"], "the pointer is on the card");
    let t2 = t1 + ms(2_000);
    model.pointer_at(Some(at(900.0, 700.0)), t2);
    model.tick(t2 + FloatKind::Peek.grace() - ms(1));
    assert_eq!(open_keys(&model), ["a"], "grace not over yet");
    model.tick(t2 + FloatKind::Peek.grace());
    assert!(model.chain().is_empty(), "moved on: dismissed");
}

#[test]
fn aiming_at_the_card_defers_a_rest_on_another_trigger_and_reaching_it_drops_that_rest() {
    let t0 = Instant::now();
    let mut model = Model::new();
    // Trigger a on the left; trigger b right below-right of it, lying on
    // the diagonal from a to the card.
    model.rest(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.tick(t0 + ms(350));
    let plate = paint_below(&mut model, "a", 392.0, 240.0);
    let mut now = t0 + ms(400);
    model.pointer_at(Some(at(130.0, 109.0)), now);
    // Leave a travelling down-right towards the card's top edge.
    now += ms(16);
    model.leave(&key("a"), now);
    model.pointer_at(Some(at(150.0, 118.0)), now);
    now += ms(16);
    model.pointer_at(Some(at(170.0, 121.0)), now);
    model.rest(req("b", b(160.0, 119.0, 40.0, 4.0), FloatKind::Peek), now);
    assert_eq!(open_keys(&model), ["a"], "b must not swap while the pointer aims at a's card");
    assert!(model.pending().is_some_and(|pending| pending.aimed));
    now += ms(16);
    model.leave(&key("b"), now);
    model.pointer_at(Some(plate.center()), now);
    model.tick(now + ms(1_000));
    assert_eq!(open_keys(&model), ["a"], "reached the card: a stays, b never opened");
    assert!(model.pending().is_none());
}

#[test]
fn stopping_on_the_other_trigger_while_aiming_swaps_after_the_aim_idle() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.rest(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.tick(t0 + ms(350));
    paint_below(&mut model, "a", 392.0, 240.0);
    let mut now = t0 + ms(400);
    model.pointer_at(Some(at(130.0, 109.0)), now);
    now += ms(16);
    model.leave(&key("a"), now);
    model.pointer_at(Some(at(150.0, 118.0)), now);
    now += ms(16);
    model.pointer_at(Some(at(170.0, 121.0)), now);
    model.rest(req("b", b(160.0, 119.0, 40.0, 4.0), FloatKind::Peek), now);
    assert_eq!(open_keys(&model), ["a"]);
    // The pointer stops on b.
    model.tick(now + AIM_IDLE);
    assert_eq!(open_keys(&model), ["b"], "stopped on b: it applies, warm");
}

#[test]
fn a_move_away_from_the_card_is_not_aim() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.rest(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.tick(t0 + ms(350));
    paint_below(&mut model, "a", 392.0, 240.0);
    let mut now = t0 + ms(400);
    model.pointer_at(Some(at(130.0, 109.0)), now);
    now += ms(16);
    model.leave(&key("a"), now);
    // Up and away from a card that hangs below.
    model.pointer_at(Some(at(135.0, 80.0)), now);
    model.rest(req("b", b(120.0, 70.0, 40.0, 18.0), FloatKind::Peek), now);
    assert_eq!(open_keys(&model), ["b"], "not aiming: warm swap at once");
}

#[test]
fn a_rest_inside_a_card_chains_three_deep_then_flags_overflow() {
    let t0 = Instant::now();
    let mut model = Model::new();
    let mut now = t0;
    model.rest(req("root", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), now);
    now += ms(350);
    model.tick(now);
    let root = paint_below(&mut model, "root", 392.0, 300.0);
    let word = |plate: Bounds<Pixels>| {
        b(
            f32::from(plate.origin.x) + 40.0,
            f32::from(plate.origin.y) + 60.0,
            70.0,
            18.0,
        )
    };
    model.pointer_at(Some(word(root).center()), now);
    model.rest(req("one", word(root), FloatKind::Peek), now);
    now += ms(350);
    model.tick(now);
    assert_eq!(open_keys(&model), ["root", "one"]);
    let one_card = model.top().expect("child").id;
    let one = b(600.0, 200.0, 330.0, 260.0);
    model.painted(one_card, one);
    model.pointer_at(Some(word(one).center()), now);
    model.rest(req("two", word(one), FloatKind::Peek), now);
    now += ms(350);
    model.tick(now);
    assert_eq!(open_keys(&model), ["root", "one", "two"]);
    assert_eq!(model.top().and_then(|card| card.level), Some(MAX_DEPTH - 1));
    let two = b(950.0, 260.0, 330.0, 260.0);
    let two_card = model.top().expect("grandchild").id;
    model.painted(two_card, two);
    model.pointer_at(Some(word(two).center()), now);
    model.rest(req("three", word(two), FloatKind::Peek), now);
    now += ms(1_000);
    model.tick(now);
    assert_eq!(open_keys(&model), ["root", "one", "two"], "a fourth never opens");
    assert!(model.top().is_some_and(|card| card.overflow), "the deepest offers open");
}

#[test]
fn esc_steps_back_exactly_one_and_the_parent_holds() {
    let t0 = Instant::now();
    let mut model = Model::new();
    let mut now = t0;
    model.rest(req("root", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), now);
    now += ms(350);
    model.tick(now);
    let root = paint_below(&mut model, "root", 392.0, 300.0);
    let word = b(f32::from(root.origin.x) + 40.0, f32::from(root.origin.y) + 60.0, 70.0, 18.0);
    model.pointer_at(Some(word.center()), now);
    model.rest(req("one", word, FloatKind::Peek), now);
    now += ms(350);
    model.tick(now);
    assert!(model.step_back(now));
    assert_eq!(open_keys(&model), ["root"]);
    // The pointer is still on the root card: it stays.
    model.tick(now + ms(2_000));
    assert_eq!(open_keys(&model), ["root"]);
    assert!(model.step_back(now + ms(2_000)));
    assert!(model.chain().is_empty());
    assert!(!model.step_back(now + ms(2_001)), "nothing left to step back");
}

#[test]
fn an_exit_reversed_mid_flight_continues_from_where_it_was() {
    let t0 = Instant::now();
    let mut model = Model::new();
    let anchor = b(100.0, 100.0, 60.0, 18.0);
    let request = req("a", anchor, FloatKind::Peek);
    let id = model.open(request.clone(), t0).expect("opened");
    let settled = t0 + ms(400);
    model.tick(settled);
    assert!((model.card(id).expect("card").presence.value(settled) - 1.0).abs() < 1e-6);
    model.close_all(settled);
    let mid = settled + FloatKind::Peek.exit() / 2;
    let before = model.card(id).expect("still leaving").presence.value(mid);
    assert!(before > 0.05 && before < 0.95, "mid-exit: {before}");
    let again = model.open(request, mid).expect("reopened");
    assert_eq!(again, id, "the leaving card is reused, not replaced");
    let after = model.card(id).expect("card").presence.value(mid);
    assert!((after - before).abs() < 1e-5, "jumped: {before} -> {after}");
    let next = model.card(id).expect("card").presence.value(mid + ms(16));
    assert!(next > after, "heads back up: {after} -> {next}");
    // It never restarts from zero at any time on the way back.
    let mut t = mid;
    while t < mid + ms(400) {
        assert!(model.card(id).expect("card").presence.value(t) >= after - 1e-5);
        t += ms(5);
    }
}

#[test]
fn a_closed_card_stays_in_the_model_until_its_exit_settles() {
    let t0 = Instant::now();
    let mut model = Model::new();
    let id = model
        .open(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0)
        .expect("opened");
    model.tick(t0 + ms(400));
    model.close_all(t0 + ms(400));
    model.tick(t0 + ms(400) + FloatKind::Peek.exit() - ms(1));
    assert!(model.card(id).is_some_and(|card| !card.is_open()), "leaving, still drawn");
    model.tick(t0 + ms(400) + FloatKind::Peek.exit());
    assert!(model.card(id).is_none(), "gone once the exit settled");
}

#[test]
fn pins_dedupe_by_key_and_newest_goes_first() {
    let t0 = Instant::now();
    let mut model = Model::new();
    let mut now = t0;
    for name in ["a", "b", "a"] {
        model.open(req(name, b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), now);
        now += ms(10);
        assert!(model.pin_top(now));
        now += ms(10);
    }
    let keys: Vec<String> = model.pins().iter().map(|pin| pin.key.to_string()).collect();
    assert_eq!(keys, ["a", "b"]);
    assert!(model.chain().is_empty(), "a pinned card leaves the pointer");
    assert!(!model.pin_top(now), "nothing open to pin");
    assert!(model.unpin(&key("b"), now));
    model.tick(now + ms(1_000));
    let keys: Vec<String> = model.pins().iter().map(|pin| pin.key.to_string()).collect();
    assert_eq!(keys, ["a"]);
}

#[test]
fn a_press_outside_closes_everything_and_a_press_inside_closes_nothing() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.open(req("menu", b(100.0, 100.0, 60.0, 18.0), FloatKind::Menu), t0);
    let plate = paint_below(&mut model, "menu", 240.0, 200.0);
    assert!(!model.press(plate.center(), t0 + ms(50)));
    assert_eq!(open_keys(&model), ["menu"]);
    assert!(model.press(at(900.0, 900.0), t0 + ms(60)));
    assert!(model.chain().is_empty());
}

#[test]
fn the_press_that_closed_a_menu_on_its_own_trigger_does_not_reopen_it() {
    let t0 = Instant::now();
    let mut model = Model::new();
    let anchor = b(100.0, 100.0, 60.0, 18.0);
    model.open(req("menu", anchor, FloatKind::Menu), t0);
    paint_below(&mut model, "menu", 240.0, 200.0);
    let press = t0 + ms(500);
    assert!(model.press(anchor.center(), press));
    // The trigger's click handler fires in the same instant.
    assert!(model.open(req("menu", anchor, FloatKind::Menu), press).is_none());
    assert!(model.chain().is_empty(), "the click toggled it shut");
    // A later click opens it again.
    assert!(model.open(req("menu", anchor, FloatKind::Menu), press + ms(300)).is_some());
}

#[test]
fn one_tip_per_window_and_tips_sweep_warm() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.rest(req("t1", b(10.0, 10.0, 20.0, 20.0), FloatKind::Tip), t0);
    model.tick(t0 + ms(449));
    assert!(model.tip().is_none());
    model.tick(t0 + ms(450));
    let id = model.tip().expect("tip").id;
    let t1 = t0 + ms(500);
    model.leave(&key("t1"), t1);
    model.rest(req("t2", b(40.0, 10.0, 20.0, 20.0), FloatKind::Tip), t1 + ms(30));
    let tip = model.tip().expect("swept");
    assert_eq!((tip.id, tip.key.clone()), (id, key("t2")));
    let open_tips = model.cards().filter(|card| card.is_open() && card.level.is_none()).count();
    assert_eq!(open_tips, 1);
}

#[test]
fn a_tracked_trigger_that_is_gone_takes_its_card_with_it() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.open(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.triggers_gone(|key| key.to_string() == "a", t0 + ms(10));
    assert!(model.chain().is_empty());
}

#[test]
fn scrolling_closes_tips_and_stale_cards_re_anchor_or_close() {
    let t0 = Instant::now();
    let mut model = Model::new();
    model.rest(req("tip", b(10.0, 10.0, 20.0, 20.0), FloatKind::Tip), t0);
    model.rest(req("a", b(100.0, 100.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.rest(req("b", b(100.0, 300.0, 60.0, 18.0), FloatKind::Peek), t0);
    model.tick(t0 + ms(460));
    assert!(model.tip().is_some());
    model.scroll(at(700.0, 700.0), t0 + ms(500));
    assert!(model.tip().is_none(), "tips close on scroll");
    let moved = b(100.0, 260.0, 60.0, 18.0);
    model.resolve_stale(|key| (key.to_string() == "b").then_some(moved), t0 + ms(516));
    assert_eq!(open_keys(&model), ["b"]);
    assert_eq!(model.top().expect("b").anchor, moved, "re-anchored to the scrolled rect");
}

// ------------------------------------------------------------------ storm

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
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
        #[allow(clippy::cast_precision_loss)]
        let value = (self.next() >> 40) as f32 / (1u64 << 24) as f32;
        value
    }
}

/// Every structural invariant the layer relies on.
fn check(model: &Model, now: Instant, step: usize) {
    let chain = model.chain();
    assert!(chain.len() <= MAX_DEPTH, "step {step}: chain {}", chain.len());
    for (index, card) in chain.iter().enumerate() {
        assert_eq!(card.level, Some(index), "step {step}: levels not contiguous");
    }
    let tips = model.cards().filter(|card| card.is_open() && card.level.is_none()).count();
    assert!(tips <= 1, "step {step}: {tips} open tips");
    let mut ids = HashSet::new();
    for card in model.cards() {
        assert!(ids.insert(card.id), "step {step}: duplicate card id");
        let value = card.presence.value(now);
        assert!((0.0..=1.0).contains(&value), "step {step}: presence {value}");
        if card.is_open() && card.kind == FloatKind::Tip {
            assert!(card.level.is_none());
        }
    }
    let mut keys = HashSet::new();
    for pin in model.pins().iter().filter(|pin| !pin.leaving) {
        assert!(keys.insert(pin.key.clone()), "step {step}: duplicate pin");
    }
    if let Some(deadline) = model.next_deadline(now) {
        assert!(deadline > now, "step {step}: a due deadline survived a tick");
    }
}

#[test]
fn storm_of_interleaved_events_keeps_the_model_consistent_and_settles_to_nothing() {
    for seed in 0..24_u64 {
        let mut rng = Rng(0xF1_0A7 ^ (seed * 7919));
        let mut model = Model::new();
        let t0 = Instant::now();
        let mut now = t0;
        let triggers: Vec<(String, Bounds<Pixels>)> = (0..12)
            .map(|index| {
                #[allow(clippy::cast_precision_loss)]
                let x = 40.0 + (index % 4) as f32 * 220.0;
                #[allow(clippy::cast_precision_loss)]
                let y = 60.0 + (index / 4) as f32 * 200.0;
                (format!("t{index}"), b(x, y, 80.0, 18.0))
            })
            .collect();
        for step in 0..3_000 {
            // Several events per "frame", then a frame's worth of time.
            for _ in 0..rng.below(5) + 1 {
                let (name, anchor) = &triggers[usize::try_from(rng.below(12)).unwrap_or(0)];
                let kind = match rng.below(6) {
                    0 => FloatKind::Tip,
                    1 => FloatKind::Lens,
                    2 => FloatKind::Menu,
                    _ => FloatKind::Peek,
                };
                // Some rests land inside open cards (chaining).
                let inside = model
                    .chain()
                    .last()
                    .and_then(|card| card.painted)
                    .filter(|_| rng.below(3) == 0);
                let anchor = inside.map_or(*anchor, |plate| {
                    b(
                        f32::from(plate.origin.x) + 20.0 + rng.unit() * 200.0,
                        f32::from(plate.origin.y) + 20.0 + rng.unit() * 100.0,
                        50.0,
                        16.0,
                    )
                });
                match rng.below(11) {
                    0..=2 => model.rest(req(name, anchor, kind), now),
                    3 | 4 => model.leave(&key(name), now),
                    5 => {
                        model.open(req(name, anchor, kind), now);
                    }
                    6 => {
                        model.step_back(now);
                    }
                    7 => {
                        model.pin_top(now);
                    }
                    8 => {
                        let target = model.pins().first().map(|pin| pin.key.clone());
                        if let Some(target) = target {
                            model.unpin(&target, now);
                        }
                    }
                    9 => {
                        let pointer = at(rng.unit() * 1000.0, rng.unit() * 800.0);
                        if rng.below(2) == 0 {
                            model.press(pointer, now);
                        } else {
                            model.scroll(pointer, now);
                            model.resolve_stale(|_| None, now);
                        }
                    }
                    _ => {
                        let pointer = at(rng.unit() * 1000.0, rng.unit() * 800.0);
                        model.pointer_at(Some(pointer), now);
                    }
                }
                check(&model, now, step);
            }
            // The layer paints every open card below its anchor.
            let open: Vec<(u64, Bounds<Pixels>, Option<usize>)> =
                model.cards().map(|card| (card.id, card.anchor, card.level)).collect();
            for (id, anchor, level) in open {
                #[allow(clippy::cast_precision_loss)]
                let shift = level.map_or(0.0, |level| level as f32 * 340.0);
                let plate = b(
                    f32::from(anchor.origin.x) + shift,
                    f32::from(anchor.origin.y + anchor.size.height) + 6.0,
                    330.0,
                    220.0,
                );
                model.painted(id, plate);
            }
            now += ms(rng.below(40));
            model.tick(now);
            check(&model, now, step);
        }
        // Neutral tail: every trigger left, the pointer gone, keyboard cards
        // dismissed; then time passes.
        for (name, _) in &triggers {
            model.leave(&key(name), now);
        }
        model.pointer_at(None, now);
        model.close_all(now);
        let pins: Vec<ElementId> = model.pins().iter().map(|pin| pin.key.clone()).collect();
        for pin in &pins {
            model.unpin(pin, now);
        }
        let mut guard = 0;
        while let Some(next) = model.next_deadline(now) {
            assert!(next > now, "seed {seed}: a deadline at or before now would spin the timer");
            now = next;
            model.tick(now);
            guard += 1;
            assert!(guard < 100, "seed {seed}: deadlines never ran out");
        }
        assert!(model.cards().next().is_none(), "seed {seed}: a card survived the tail");
        assert!(model.pins().is_empty(), "seed {seed}: a pin survived");
        assert!(model.is_settled(now), "seed {seed}: not settled");
    }
}
