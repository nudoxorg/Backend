//! Storms: seeded, reproducible random input scripts, the neutral tail that
//! ends them, the calm replay a fresh boot is compared against, and a
//! shrinker that turns a failing storm into the smallest script that still
//! fails the same way.
//!
//! Everything here is pure data: a storm is a function of its seed and its
//! vocabulary, so `seed` + scene is a complete bug report.

use crate::script::{Act, Button, Event, Mods, Script};

/// A small, fast, seedable generator (`SplitMix64`); the harness's only
/// randomness.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator for `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform integer in `0..n` (0 when `n` is 0).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// A uniform float in `0..1`.
    #[allow(clippy::cast_precision_loss)]
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1_u64 << 24) as f32
    }

    /// A uniform float in `low..high`.
    pub fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.unit()
    }

    /// One of `items` (None when empty).
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        let len = u64::try_from(items.len()).unwrap_or(0);
        items.get(usize::try_from(self.below(len)).unwrap_or(0))
    }

    /// Whether an event of probability `p` happens.
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

/// What a storm may do, and how often (relative weights; 0 disables).
#[derive(Clone, Debug, PartialEq)]
pub struct Vocabulary {
    /// Logical size of the window when the storm starts.
    pub size: (u32, u32),
    /// Points of interest (centres of published targets and texts): most
    /// pointer acts aim here.
    pub targets: Vec<(f32, f32)>,
    /// Key chords the scene answers to.
    pub chords: Vec<String>,
    /// Widths a resize may pick (heights scale with the start aspect).
    pub widths: Vec<u32>,
    /// Text scales a flip may pick.
    pub text_scales: Vec<u16>,
    /// Pointer moves.
    pub moves: u32,
    /// Clicks.
    pub clicks: u32,
    /// Button down now, up later (drags, presses held across frames).
    pub presses: u32,
    /// Wheel deltas.
    pub scrolls: u32,
    /// Key chords.
    pub keys: u32,
    /// Modifier holds and releases.
    pub holds: u32,
    /// Window resizes.
    pub resizes: u32,
    /// Text-scale, density, theme, contrast and motion flips.
    pub settings: u32,
    /// Pointer leaving the window.
    pub leaves: u32,
    /// Drags (interpolated at the frame period).
    pub drags: u32,
    /// Pinch zooms.
    pub zooms: u32,
    /// Chance that the next act lands in the same instant (a burst).
    pub burst: f32,
}

impl Vocabulary {
    /// The default mix for a window of `size`.
    #[must_use]
    pub fn new(size: (u32, u32)) -> Self {
        Self {
            size,
            targets: Vec::new(),
            chords: [
                "tab",
                "shift-tab",
                "escape",
                "enter",
                "space",
                "j",
                "k",
                "up",
                "down",
                "left",
                "right",
                "cmd-k",
            ]
            .map(str::to_owned)
            .to_vec(),
            widths: vec![480, 640, 760, 900, 1100, 1280, 1440],
            text_scales: vec![85, 100, 125, 150, 200],
            moves: 30,
            clicks: 12,
            presses: 5,
            scrolls: 4,
            keys: 16,
            holds: 6,
            resizes: 4,
            settings: 5,
            leaves: 2,
            drags: 3,
            zooms: 2,
            burst: 0.3,
        }
    }

    fn weights(&self) -> [u32; 11] {
        [
            self.drags,
            self.zooms,
            self.moves,
            self.clicks,
            self.presses,
            self.scrolls,
            self.keys,
            self.holds,
            self.resizes,
            self.settings,
            self.leaves,
        ]
    }
}

/// Generates a storm of `acts` acts over roughly `span_ms`, from `seed`.
/// Holds are always released and presses always lifted later in the same
/// storm or by the neutral tail.
#[must_use]
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
pub fn generate(seed: u64, vocabulary: &Vocabulary, acts: usize, span_ms: u64) -> Script {
    let mut rng = Rng::new(seed);
    let mut script = Script::new();
    let mut at = 0_u64;
    let mut size = vocabulary.size;
    let mut held = Mods::default();
    let mut pressed: Option<Button> = None;
    let mean_gap = (span_ms / u64::try_from(acts.max(1)).unwrap_or(1)).max(1);
    let weights = vocabulary.weights();
    let total: u32 = weights.iter().sum();
    let point = |rng: &mut Rng, size: (u32, u32)| -> (f32, f32) {
        let aim = rng.chance(0.65).then(|| rng.pick(&vocabulary.targets).copied()).flatten();
        let (x, y) = aim.unwrap_or_else(|| {
            (
                rng.range(0.0, size.0 as f32),
                rng.range(0.0, size.1 as f32),
            )
        });
        // Jitter inside the target, clamp inside the window.
        let x = (x + rng.range(-6.0, 6.0)).clamp(0.0, size.0 as f32 - 1.0);
        let y = (y + rng.range(-4.0, 4.0)).clamp(0.0, size.1 as f32 - 1.0);
        (x.round(), y.round())
    };
    for _ in 0..acts {
        if !rng.chance(vocabulary.burst) {
            at += 1 + rng.below(mean_gap * 2);
        }
        let mut roll = u32::try_from(rng.below(u64::from(total.max(1)))).unwrap_or(0);
        let mut kind = 0;
        for (index, weight) in weights.iter().enumerate() {
            if roll < *weight {
                kind = index;
                break;
            }
            roll -= weight;
        }
        match kind {
            0 => {
                let (from_x, from_y) = point(&mut rng, size);
                let (to_x, to_y) = point(&mut rng, size);
                if let Some(button) = pressed.take() {
                    script.push(
                        at,
                        Act::Up {
                            x: from_x,
                            y: from_y,
                            button,
                        },
                    );
                }
                script.push(
                    at,
                    Act::Drag {
                        from_x,
                        from_y,
                        to_x,
                        to_y,
                        over_ms: 40 + rng.below(200),
                        button: Button::Left,
                    },
                );
            }
            1 => {
                let (x, y) = point(&mut rng, size);
                let factor = *rng.pick(&[0.5_f32, 0.8, 1.25, 2.0]).unwrap_or(&1.25);
                script.push(at, Act::Zoom { x, y, factor });
            }
            2 => {
                let (x, y) = point(&mut rng, size);
                script.push(at, Act::Move { x, y });
            }
            3 => {
                let (x, y) = point(&mut rng, size);
                if let Some(button) = pressed.take() {
                    script.push(at, Act::Up { x, y, button });
                }
                let button = if rng.chance(0.9) {
                    Button::Left
                } else {
                    Button::Right
                };
                script.push(at, Act::Click { x, y, button });
            }
            4 => {
                let (x, y) = point(&mut rng, size);
                match pressed.take() {
                    Some(button) => script.push(at, Act::Up { x, y, button }),
                    None => {
                        pressed = Some(Button::Left);
                        script.push(
                            at,
                            Act::Down {
                                x,
                                y,
                                button: Button::Left,
                            },
                        );
                    }
                }
            }
            5 => {
                let (x, y) = point(&mut rng, size);
                let dy = rng.range(-240.0, 240.0).round();
                script.push(at, Act::Scroll { x, y, dx: 0.0, dy });
            }
            6 => {
                if let Some(chord) = rng.pick(&vocabulary.chords) {
                    script.push(
                        at,
                        Act::Key {
                            chord: chord.clone(),
                        },
                    );
                }
            }
            7 => {
                let choice = *rng
                    .pick(&[
                        Mods {
                            cmd: true,
                            ..Mods::default()
                        },
                        Mods {
                            alt: true,
                            ..Mods::default()
                        },
                        Mods {
                            shift: true,
                            ..Mods::default()
                        },
                    ])
                    .unwrap_or(&Mods::default());
                if held.with(choice) == held {
                    held = held.without(choice);
                    script.push(at, Act::Release { mods: choice });
                } else {
                    held = held.with(choice);
                    script.push(at, Act::Hold { mods: choice });
                }
            }
            8 => {
                let width = *rng.pick(&vocabulary.widths).unwrap_or(&size.0);
                let aspect = vocabulary.size.1 as f32 / vocabulary.size.0.max(1) as f32;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let height = ((width as f32 * aspect).round() as u32).clamp(320, 1600);
                size = (width, height);
                script.push(at, Act::Resize { width, height });
            }
            9 => {
                let act = match rng.below(5) {
                    0 => Act::TextScale {
                        percent: *rng.pick(&vocabulary.text_scales).unwrap_or(&100),
                    },
                    1 => Act::Density {
                        name: rng
                            .pick(&["comfortable", "compact", "dense"])
                            .map_or("comfortable", |name| name)
                            .to_owned(),
                    },
                    2 => Act::Theme {
                        name: rng
                            .pick(&["abyss", "glacier"])
                            .map_or("abyss", |name| name)
                            .to_owned(),
                    },
                    3 => Act::Contrast {
                        name: rng
                            .pick(&["normal", "high"])
                            .map_or("normal", |name| name)
                            .to_owned(),
                    },
                    _ => Act::Motion {
                        on: rng.chance(0.7),
                    },
                };
                script.push(at, act);
            }
            _ => script.push(at, Act::Leave),
        }
    }
    script
}

/// The acts that end every storm: modifiers up, any held button up, the
/// pointer out of the window, then `escapes` presses of Esc (spaced by
/// `gap_ms`), starting at `at_ms`.
#[must_use]
pub fn neutral_tail(at_ms: u64, escapes: usize, gap_ms: u64) -> Script {
    let mut tail = Script::new();
    tail.push(at_ms, Act::Release { mods: Mods::ALL });
    tail.push(at_ms, Act::Leave);
    for index in 0..escapes {
        tail.push(
            at_ms + gap_ms * (u64::try_from(index).unwrap_or(0) + 1),
            Act::Key {
                chord: "escape".to_owned(),
            },
        );
    }
    tail
}

/// Lifts any button the script leaves held, at `at_ms` (the pointer's last
/// position is used).
#[must_use]
pub fn release_buttons(script: &Script, at_ms: u64) -> Script {
    let mut held: Option<(Button, f32, f32)> = None;
    let mut last = (0.0, 0.0);
    for event in &script.events {
        match &event.act {
            Act::Move { x, y } | Act::Click { x, y, .. } | Act::Scroll { x, y, .. } => {
                last = (*x, *y);
            }
            Act::Down { x, y, button } => {
                last = (*x, *y);
                held = Some((*button, *x, *y));
            }
            Act::Up { x, y, .. } => {
                last = (*x, *y);
                held = None;
            }
            _ => {}
        }
    }
    let mut out = Script::new();
    if let Some((button, _, _)) = held {
        out.push(
            at_ms,
            Act::Up {
                x: last.0,
                y: last.1,
                button,
            },
        );
    }
    out
}

/// The calm replay of a storm: every act that can change what the scene
/// shows at rest (clicks, presses, keys, text, scrolls, holds, resizes,
/// settings), in order, one per `gap_ms`, without the pointer wandering in
/// between. A fresh boot playing this must settle to the same pixels as the
/// storm did.
#[must_use]
pub fn calm(storm: &Script, gap_ms: u64) -> Script {
    let mut out = Script::new();
    let mut at = 0_u64;
    for event in &storm.events {
        match event.act {
            Act::Move { .. } | Act::Leave => {}
            _ => {
                out.push(at, event.act.clone());
                at += gap_ms;
            }
        }
    }
    out
}

/// Shrinks a failing script: the shortest failing prefix first, then
/// delta debugging (removing chunks of events while the failure stays the
/// same), within `budget` runs of `fails`.
pub fn shrink(
    script: &Script,
    budget: usize,
    fails: &mut dyn FnMut(&Script) -> bool,
) -> (Script, usize) {
    let mut runs = 0_usize;
    let mut events = script.events.clone();
    // Shortest failing prefix (binary search on the prefix length).
    let (mut low, mut high) = (0_usize, events.len());
    while low < high && runs < budget {
        let middle = (low + high) / 2;
        runs += 1;
        if fails(&Script {
            events: events[..middle].to_vec(),
        }) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    events.truncate(high.max(1).min(events.len()));
    // Delta debugging on what is left.
    let mut chunk = events.len().div_ceil(2).max(1);
    while chunk >= 1 && runs < budget && events.len() > 1 {
        let mut removed_any = false;
        let mut start = 0;
        while start < events.len() && runs < budget {
            let end = (start + chunk).min(events.len());
            let candidate: Vec<Event> = events[..start]
                .iter()
                .chain(&events[end..])
                .cloned()
                .collect();
            if candidate.is_empty() {
                start = end;
                continue;
            }
            runs += 1;
            if fails(&Script {
                events: candidate.clone(),
            }) {
                events = candidate;
                removed_any = true;
            } else {
                start = end;
            }
        }
        if chunk == 1 && !removed_any {
            break;
        }
        if !removed_any {
            chunk = chunk.div_ceil(2).max(1);
            if chunk == 1 && events.len() > 1 {
                continue;
            }
        }
    }
    (Script { events }, runs)
}

#[cfg(test)]
mod tests {
    use super::{Rng, Vocabulary, calm, generate, neutral_tail, shrink};
    use crate::script::{Act, Script};

    #[test]
    fn the_same_seed_gives_the_same_storm_and_another_seed_does_not() {
        let mut vocabulary = Vocabulary::new((720, 440));
        vocabulary.targets = vec![(110.0, 114.0), (270.0, 114.0)];
        let a = generate(7, &vocabulary, 80, 2_000);
        let b = generate(7, &vocabulary, 80, 2_000);
        let c = generate(8, &vocabulary, 80, 2_000);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.events.len() >= 80, "{}", a.events.len());
        // Times ascend, bursts exist, and it round-trips as text.
        assert!(a.events.windows(2).all(|w| w[0].at_ms <= w[1].at_ms));
        assert!(a.events.windows(2).any(|w| w[0].at_ms == w[1].at_ms));
        assert_eq!(Script::parse(&a.to_string()).expect("reparses"), a);
    }

    #[test]
    fn rng_is_uniform_enough_to_reach_every_bucket() {
        let mut rng = Rng::new(1);
        let mut buckets = [0_u32; 10];
        for _ in 0..10_000 {
            buckets[usize::try_from(rng.below(10)).unwrap_or(0)] += 1;
        }
        assert!(buckets.iter().all(|count| (800..1200).contains(count)), "{buckets:?}");
    }

    #[test]
    fn calm_replay_drops_only_pointer_wandering_and_spaces_the_rest() {
        let storm = Script::parse(
            "move 1,1 @0; click 5,5 @0; move 9,9 @3; key tab @4; leave @5; resize 500x400 @5",
        )
        .expect("parses");
        let replay = calm(&storm, 500);
        assert_eq!(
            replay.to_string(),
            "click left 5,5 @0\nkey tab @500\nresize 500x400 @1000"
        );
        let tail = neutral_tail(2_000, 2, 60);
        assert_eq!(
            tail.to_string(),
            "release all @2000\nleave @2000\nkey escape @2060\nkey escape @2120"
        );
    }

    #[test]
    fn shrinking_finds_the_one_act_that_matters() {
        let mut vocabulary = Vocabulary::new((720, 440));
        vocabulary.targets = vec![(100.0, 100.0)];
        let mut storm = generate(3, &vocabulary, 60, 2_000);
        storm.push(
            900,
            Act::Key {
                chord: "x".to_owned(),
            },
        );
        let culprit = |script: &Script| {
            script
                .events
                .iter()
                .any(|event| event.act == Act::Key { chord: "x".to_owned() })
        };
        let (small, runs) = shrink(&storm, 200, &mut |script| culprit(script));
        assert_eq!(small.events.len(), 1, "{small}");
        assert!(culprit(&small));
        assert!(runs < 200);
    }
}
