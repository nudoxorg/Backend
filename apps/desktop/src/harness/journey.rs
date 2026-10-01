//! Journeys: the end-to-end acceptance (gui-plan §3 item 10). A journey is a
//! person's path through the real desktop, scripted as steps with content
//! checks at named checkpoints, filmed, and judged.
//!
//! ```text
//! backend-desktop-gui-harness journey NAME|FILE [--out DIR] [--scale 1|2] [--no-fresh]
//!                                               [--index DIR] [--remake-states] [--machine fixture]
//! ```
//!
//! `NAME` is `apps/desktop/journeys/NAME.journey`; parts are
//! `apps/desktop/journeys/parts/*.part` ([`parts`]). The run writes
//! `REPORT.txt` (each step and checkpoint with quoted evidence, budgets,
//! settle == fresh, the verdict), `strip.png` (every checkpoint frame and the
//! motion between), `motion.json`, `checks.txt` (every text and target at
//! each checkpoint), `resolved.txt` (the acts as delivered) and each
//! checkpoint's frame. It exits 0 on PASS, 1 on FAIL, 3 on BLOCKED.
//!
//! **The machine** ([`plan::Start`], the header):
//! - `start clean`: no state dir, no settings, no projects; the production
//!   launch path ([`machine`]) on the live user root.
//! - `start from PART ARGS [then PART ARGS]…`: the end state of those parts
//!   run from clean, materialized once and cached by key ([`state`]); the
//!   REPORT names the state and its key.
//! - `start ROUTE`: the scenes' fixture index, roots pre-admitted, booted at
//!   ROUTE (`--index DIR` keeps its own copy). Not an install.
//!
//! **Lines** (one per line; `#` comments; `size WxH` header):
//!
//! | line | meaning |
//! |---|---|
//! | any `storm --replay` act, untimed | `key cmd-k`, `type "toml Value"`, `resize 480x900`, `hold alt`: delivered between two frames. `route …` and the settings acts inject intents, so they may only be a detour |
//! | `click PICK`, `hover PICK` | settle, then the pointer goes to what a person points at: `"WORDS" [after "ANCHOR"…] [in AREA]` or a probe id (`*` globs) |
//! | `STEP else STEP` | the detour is taken only when the step could not be done; the step still fails |
//! | `settle`, `wait MS` | frames until still; virtual time passing |
//! | `await text\|absent "S"… [in AREA] within 20m` | real time passes, frames drawn, until the owner's work shows it; every new text in the area is logged as it appears (the stages a person saw) |
//! | `answer-picker PATH…\|cancel` | the native folder panel the product opened is answered: the one substitution a journey may make. Fails when no panel is open |
//! | `restart` | quit (quit handlers run, the owner stops) and launch again on the same machine |
//! | `do PART ARGS…` | a part, expanded in place; its checks run here |
//! | `needs product\|data "WHAT"` | gates the next step: if it fails, the journey is BLOCKED(product/data: WHAT); a gated act without a detour stops the walk there, a gated check does not (it changed nothing) |
//! | `check NAME` + indented asserts | settle, capture, judge |
//!
//! Asserts, on the settled frame, content first: `route WORDS` (fixture
//! names), `route like "GLOB"` (the route's exact words, globbed: the
//! production check), `text "S"… [in AREA]`, `saw "S"… [in AREA]` (on screen
//! in some frame since the previous checkpoint), `order`, `absent`, `line`,
//! `link PICK`, `focus PICK`, `focus restored`, and `budget page-open|search|
//! flight|frame-p95 <= N ms`. Areas come from the shell's resolved frame.
//! Every checkpoint also requires zero lints.
//!
//! Budgets: `page-open` and `search` time the last activation to its first
//! complete frame (real read waits plus virtual time plus the draw);
//! `flight` to the settle; `frame-p95` over every frame. A debug build
//! measures but does not judge them.
//!
//! Settle == fresh: the journey ends with the pointer leaving and a settle;
//! the same start machine is then prepared again and the performed steps are
//! replayed calmly (each settled before the next), and the two settled frames
//! must be pixel-identical.

pub mod crawl;
pub mod keys;
mod look;
mod machine;
pub mod parts;
pub mod plan;
pub mod script;
pub mod state;

pub use parts::Parts;
pub use plan::{Builder, Plan, Recipe, Start};
pub use script::{Area, Assert, Budget, Gap, GapKind, Pick, PickerAnswer, Step, StepKind, Tone, Until};

use super::{Booted, adapt, boot, fixture, quiet, route};
use crate::navigation::Route;
use backend_gui_harness::{Act, Event, Quiet, Script, Session, SessionOptions, Viewport};
use facet::gallery::align::{self, Observed, Tolerance};
use facet::gallery::json::Json;
use facet::gallery::{self, SceneRoot, compose, lint};
use facet::probe;
use gpui::{AppContext as _, IntoElement, Render};
use image::RgbaImage;
use look::{Seen, area_words, clip, describe, short};
use machine::{FRAME_MS, Launched};
use state::{StateKey, Store};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long a settle may take before the step fails.
const SETTLE_CAP_MS: u64 = 4_000;
/// Film frames after each act (offsets in virtual ms), taken while moving.
const FILM_OFFSETS: [u64; 6] = [0, 96, 224, 416, 640, 960];
/// Strip tile width in physical px.
const TILE_WIDTH: u32 = 960;
/// Budgets are judged only where they mean something.
const JUDGE_BUDGETS: bool = !cfg!(debug_assertions);
/// Real time between frames while awaiting the owner.
const AWAIT_POLL: Duration = Duration::from_millis(250);
/// Real time between film tiles while awaiting (only when the area changed).
const AWAIT_FILM: Duration = Duration::from_secs(20);
/// At most this many new texts are logged per await.
const AWAIT_LOG: usize = 80;

/// The last activation, being timed.
struct Anchor {
    label: String,
    at_ms: u64,
    wall: Duration,
    open_ms: Option<f64>,
    settled_ms: Option<u64>,
}

/// Film frames due after the last act.
struct Film {
    label: String,
    at_ms: u64,
    next: usize,
}

struct Tile {
    label: String,
    image: RgbaImage,
}

/// A checkpoint's outcome.
struct Judged {
    name: String,
    origin: String,
    at_ms: u64,
    passed: bool,
    /// Its content (every assert and the settle) held; only lints failed
    /// when `passed` is false and this is true.
    content: bool,
    /// The gap this checkpoint waits on, when it is gated.
    needs: Option<Gap>,
    lines: Vec<String>,
}

/// What the run did, in order, for the calm replay.
#[derive(Clone, Debug)]
enum Performed {
    Acts(Vec<Act>),
    Answer(PickerAnswer),
    Await { until: Until, within: Duration },
    Restart,
}

struct Nothing;

impl Render for Nothing {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn landed(cx: &gpui::App) -> u64 {
    cx.try_global::<Booted>()
        .map_or(0, |booted| booted.graph.store.read(cx).stats().landed)
}

fn current_route(cx: &gpui::App) -> Option<Route> {
    cx.try_global::<Booted>()
        .map(|booted| booted.graph.store.read(cx).snapshot().route().clone())
}

fn shell_frame(cx: &gpui::App) -> Option<crate::shell::Frame> {
    cx.try_global::<Booted>()
        .and_then(|booted| booted.shell.read(cx).frame())
}

/// Opens a window at `size` on the fixture, booted at `start`, and waits
/// until its first reads have landed.
fn open_fixture(start: &str, size: (u32, u32), scale: u8) -> Result<Session, String> {
    facet::fonts::verify().map_err(err)?;
    fixture()?;
    let viewport = Viewport::new(size.0, size.1, scale).map_err(err)?;
    let failure = Rc::new(RefCell::new(None::<String>));
    let start = start.to_owned();
    let facet = facet::Facet::default();
    let mut session = Session::open(
        viewport,
        SessionOptions {
            asset_source: std::sync::Arc::new(facet::icons::Assets),
            frame_ms: FRAME_MS,
        },
        {
            let failure = Rc::clone(&failure);
            move |window, cx| {
                if let Err(error) = gallery::bootstrap(facet, true, cx) {
                    *failure.borrow_mut() = Some(error.0);
                }
                let view = match boot(&start, window, cx) {
                    Ok(view) => view,
                    Err(error) => {
                        *failure.borrow_mut() = Some(format!("boot at `{start}`: {error}"));
                        cx.new(|_| Nothing).into()
                    }
                };
                let root = cx.new(|cx| SceneRoot::new(view, cx));
                cx.new(|cx| gpui_component::Root::new(root, window, cx).bordered(false))
            }
        },
    )
    .map_err(err)?;
    if let Some(error) = failure.borrow_mut().take() {
        return Err(error);
    }
    session.set_quiet(Some(Quiet {
        check: Box::new(quiet),
        deadline: Duration::from_secs(120),
    }));
    session.quiesce().map_err(err)?;
    session
        .update(|_, cx| {
            let _ = probe::take(cx);
        })
        .map_err(err)?;
    Ok(session)
}

/// The areas a text can be seen in, as bits: bit 0 is "anywhere visible".
const AREAS: [Area; 5] = [Area::Titlebar, Area::Shelf, Area::Reader, Area::Pins, Area::Status];

fn area_bit(area: Option<Area>) -> u8 {
    area.map_or(1, |area| 1 << (1 + AREAS.iter().position(|each| *each == area).unwrap_or(0)))
}

struct Runner {
    session: Option<Session>,
    /// The production launch the session belongs to.
    launched: Option<Launched>,
    /// Where production launches run (the live user root).
    live: Option<PathBuf>,
    size: (u32, u32),
    scale: u8,
    /// Virtual time before the current session (restarts start a new clock).
    epoch_ms: u64,
    frames: usize,
    last: Option<Seen>,
    moving: bool,
    observed: Vec<Observed>,
    cpu_ms: Vec<f64>,
    events: Vec<Event>,
    performed: Vec<Performed>,
    undrawn: usize,
    anchor: Option<Anchor>,
    film: Option<Film>,
    tiles: Vec<Tile>,
    filming: bool,
    /// How each route (as exact words) was last left.
    left_by: HashMap<String, Leave>,
    /// Every text on screen since the previous checkpoint, with the areas
    /// it was seen in (`area_bit`).
    saw: BTreeMap<String, u8>,
}

/// A production launch ends the way a quit does ([`machine::quit`]), on
/// every path out of the run: an error in the middle of a journey must be
/// reported, not turned into gpui's leaked-handle panic by dropping an app
/// whose owner watch still holds the root (GAPS.md D2).
impl Drop for Runner {
    fn drop(&mut self) {
        if let (Some(session), Some(launched)) = (self.session.take(), self.launched.take()) {
            machine::quit(session, launched);
        }
    }
}

impl Runner {
    fn new(session: Session, launched: Option<Launched>, live: Option<PathBuf>, size: (u32, u32), scale: u8, filming: bool) -> Self {
        Self {
            session: Some(session),
            launched,
            live,
            size,
            scale,
            epoch_ms: 0,
            frames: 0,
            last: None,
            moving: true,
            observed: Vec::new(),
            cpu_ms: Vec::new(),
            events: Vec::new(),
            performed: Vec::new(),
            undrawn: 0,
            anchor: None,
            film: None,
            tiles: Vec::new(),
            filming,
            left_by: HashMap::new(),
            saw: BTreeMap::new(),
        }
    }

    fn session(&mut self) -> Result<&mut Session, String> {
        self.session.as_mut().ok_or_else(|| "the window is closed".to_owned())
    }

    /// The current route as exact words.
    fn route_words(&mut self) -> Result<String, String> {
        let route = self.session()?.update(|_, cx| current_route(cx)).map_err(err)?;
        Ok(route.as_ref().map_or_else(|| "(no route)".to_owned(), describe))
    }

    fn route(&mut self) -> Result<Option<Route>, String> {
        self.session()?.update(|_, cx| current_route(cx)).map_err(err)
    }

    fn now(&self) -> u64 {
        self.epoch_ms + self.session.as_ref().map_or(0, Session::now_ms)
    }

    fn film_due(&mut self, at: u64) -> Option<String> {
        if !self.filming {
            return None;
        }
        let moving = self.moving;
        let film = self.film.as_mut()?;
        let offset = at.saturating_sub(film.at_ms);
        let due = FILM_OFFSETS.get(film.next).copied()?;
        if offset < due {
            return None;
        }
        film.next += 1;
        (film.next == 1 || moving).then(|| format!("t={at} ms  +{offset}  {}", film.label))
    }

    /// Draws the next frame (the first one at the current time).
    fn tick(&mut self, force: bool) -> Result<Option<RgbaImage>, String> {
        let epoch = self.epoch_ms;
        let at = if self.frames == 0 {
            self.now()
        } else {
            (self.now() / FRAME_MS + 1) * FRAME_MS
        };
        let session = self.session()?;
        session.advance_to(at.saturating_sub(epoch));
        let started = Instant::now();
        session.quiesce().map_err(err)?;
        let waited = started.elapsed();
        if let Some(anchor) = self.anchor.as_mut()
            && anchor.open_ms.is_none()
        {
            anchor.wall += waited;
        }
        let film = self.film_due(at);
        let capture = force || film.is_some();
        let session = self.session()?;
        let (mut drawn, image) = session.frame(capture).map_err(err)?;
        drawn.at_ms += epoch;
        let (ledger, complete, frame, state, painted, words) = session
            .update(|window, cx| {
                let ledger = probe::take(cx);
                let before = landed(cx);
                let idle = quiet(cx);
                let state = super::sample_state(cx, &ledger);
                let painted = look::painted_extras(&ledger, window.painted_texts());
                let words = look::state_words(cx);
                (ledger, idle && landed(cx) == before, shell_frame(cx), state, painted, words)
            })
            .map_err(err)?;
        self.frames += 1;
        let cpu = drawn.cpu.as_secs_f64() * 1000.0;
        self.cpu_ms.push(cpu);
        if let Some(anchor) = self.anchor.as_mut()
            && anchor.open_ms.is_none()
            && complete
        {
            #[allow(clippy::cast_precision_loss, reason = "virtual ms stay far below 2^52")]
            let virtual_ms = drawn.at_ms.saturating_sub(anchor.at_ms) as f64;
            anchor.open_ms = Some(anchor.wall.as_secs_f64() * 1000.0 + virtual_ms + cpu);
        }
        let mut stripped = ledger.clone();
        stripped.texts.clear();
        stripped.targets.clear();
        stripped.stacks.clear();
        stripped.scrolls.clear();
        // A relaunch is a new app: its motion clock starts again at 0, and
        // what it animates is not the quit app's element carried on. Its
        // tracks are put on the journey's one timeline and named apart, so
        // the checks judge each launch's motion on its own.
        if epoch > 0 {
            #[allow(clippy::cast_precision_loss, reason = "virtual ms stay far below 2^52")]
            let shift = epoch as f64;
            for track in &mut stripped.tracks {
                track.key = format!("{}@{}", track.key, epoch);
                track.at_ms += shift;
                track.started_ms += shift;
            }
        }
        self.observed.push(Observed {
            drawn,
            ledger: stripped,
            events: self.events.len() - self.undrawn,
            state: Some(state),
        });
        self.undrawn = self.events.len();
        self.moving = !complete || ledger.any_live() || drawn.invalidations > 0;
        if let (Some(label), Some(image)) = (film, image.as_ref()) {
            self.tiles.push(Tile {
                label,
                image: tile(image),
            });
        }
        let seen = Seen {
            drawn,
            ledger,
            complete,
            frame,
            painted,
            state: words,
        };
        for area in std::iter::once(None).chain(AREAS.into_iter().map(Some)) {
            for text in seen.texts(area) {
                *self.saw.entry(text.content.clone()).or_default() |= area_bit(area);
            }
        }
        self.last = Some(seen);
        Ok(image)
    }

    /// Frames until two in a row are complete, still, and unrequested.
    /// `Err(words)` says what never settled.
    fn settle(&mut self) -> Result<Result<u64, String>, String> {
        let from = self.now();
        let mut calm_since = None;
        loop {
            self.tick(false)?;
            let now = self.now();
            let Some(seen) = self.last.as_ref() else {
                continue;
            };
            let still = seen.complete && !seen.ledger.any_live() && seen.drawn.invalidations == 0;
            if still {
                let since = *calm_since.get_or_insert(seen.drawn.at_ms);
                if seen.drawn.at_ms > since {
                    if let Some(anchor) = self.anchor.as_mut()
                        && anchor.settled_ms.is_none()
                    {
                        anchor.settled_ms = Some(since.saturating_sub(anchor.at_ms));
                    }
                    return Ok(Ok(since));
                }
            } else {
                calm_since = None;
            }
            if now.saturating_sub(from) > SETTLE_CAP_MS {
                let live = seen
                    .ledger
                    .tracks
                    .iter()
                    .filter(|track| track.live)
                    .map(|track| track.key.clone())
                    .collect::<Vec<_>>();
                return Ok(Err(format!(
                    "not still {SETTLE_CAP_MS} ms after {from} ms: live tracks [{}], {} invalidations in the last frame, reads {}",
                    live.join(", "),
                    seen.drawn.invalidations,
                    if seen.complete { "landed" } else { "still in flight" }
                )));
            }
        }
    }

    /// Delivers acts at the current instant, then draws the next frame.
    fn deliver(&mut self, acts: &[Act], label: &str) -> Result<(), String> {
        let at = self.now();
        let started = Instant::now();
        let mut typed = false;
        for act in acts {
            self.session()?
                .apply(act, &mut |act, window, cx| adapt(act, window, cx))
                .map_err(err)?;
            self.events.push(Event {
                at_ms: at,
                act: act.clone(),
            });
            typed |= matches!(act, Act::Type { .. });
        }
        self.performed.push(Performed::Acts(acts.to_vec()));
        let wall = if typed { Duration::ZERO } else { started.elapsed() };
        if acts.iter().any(|act| act.is_activation() || act.is_setting()) {
            self.anchor = Some(Anchor {
                label: label.to_owned(),
                at_ms: at,
                wall,
                open_ms: None,
                settled_ms: None,
            });
        }
        self.film = Some(Film {
            label: label.to_owned(),
            at_ms: at,
            next: 0,
        });
        self.tick(false)?;
        Ok(())
    }

    fn seen(&self) -> Result<&Seen, String> {
        self.last.as_ref().ok_or_else(|| "no frame drawn yet".to_owned())
    }

    /// Quit and launch again on the same live root.
    fn restart(&mut self) -> Result<(), String> {
        let live = self.live.clone().ok_or_else(|| "`restart` needs the production machine".to_owned())?;
        let (session, launched) = (self.session.take(), self.launched.take());
        let before = self.now();
        if let (Some(session), Some(launched)) = (session, launched) {
            machine::quit(session, launched);
        }
        let (session, launched) = machine::open_production(&live, self.size, self.scale)?;
        self.epoch_ms = before + FRAME_MS;
        self.session = Some(session);
        self.launched = Some(launched);
        self.frames = 0;
        self.anchor = None;
        self.film = Some(Film { label: "relaunch".to_owned(), at_ms: self.epoch_ms, next: 0 });
        self.left_by.clear();
        self.performed.push(Performed::Restart);
        Ok(())
    }

    /// Frames, in real time, until the owner's work shows `until`.
    fn await_until(&mut self, until: &Until, within: Duration, report: &mut String) -> Result<Result<(), String>, String> {
        let started = Instant::now();
        let area = match until {
            Until::Text(_, area) | Until::Absent(_, area) | Until::Like(_, area) => *area,
        };
        let mut shown = std::collections::BTreeSet::new();
        let mut logged = 0;
        let mut last_film = Instant::now();
        let mut filmed = Vec::new();
        self.performed.push(Performed::Await { until: until.clone(), within });
        loop {
            self.tick(false)?;
            let texts = self.seen()?.texts(area).iter().map(|text| text.content.clone()).collect::<Vec<_>>();
            for text in &texts {
                if shown.insert(text.clone()) && logged < AWAIT_LOG && !started.elapsed().is_zero() {
                    logged += 1;
                    let _ = writeln!(report, "           +{:>6.1} s  \"{}\"", started.elapsed().as_secs_f64(), clip(&short(text), 100));
                }
            }
            let holds = match until {
                Until::Text(words, _) => words.iter().all(|word| texts.contains(word)),
                Until::Absent(words, _) => words.iter().all(|word| !texts.contains(word)),
                Until::Like(glob, _) => texts.iter().any(|text| script::glob(glob, text)),
            };
            if holds {
                let _ = writeln!(report, "           held after {:.1} s real ({} frames)", started.elapsed().as_secs_f64(), self.frames);
                return Ok(Ok(()));
            }
            if started.elapsed() > within {
                return Ok(Err(format!(
                    "not true after {:.0} s real; {}",
                    started.elapsed().as_secs_f64(),
                    self.seen()?.summary(area)
                )));
            }
            if self.filming && last_film.elapsed() > AWAIT_FILM && texts != filmed {
                last_film = Instant::now();
                filmed.clone_from(&texts);
                let at = self.now();
                if let Some(image) = self.tick(true)? {
                    self.tiles.push(Tile { label: format!("t={at} ms  awaiting, +{:.0} s real", started.elapsed().as_secs_f64()), image: tile(&image) });
                }
            }
            std::thread::sleep(AWAIT_POLL);
        }
    }
}

fn tile(image: &RgbaImage) -> RgbaImage {
    if image.width() <= TILE_WIDTH {
        return image.clone();
    }
    let height = u32::try_from(
        u64::from(image.height()) * u64::from(TILE_WIDTH) / u64::from(image.width().max(1)),
    )
    .unwrap_or(1)
    .max(1);
    image::imageops::resize(image, TILE_WIDTH, height, image::imageops::FilterType::Triangle)
}

fn expected_route(words: &str, production: bool) -> Result<String, String> {
    if production {
        return Err("`route WORDS` resolves names on the fixture index; the production machine checks `route like \"GLOB\"`".to_owned());
    }
    let target = route::parse(words)?;
    route::to_route(&target, fixture()?).map(|route| describe(&route))
}

/// Every `route` act resolves against the fixture index (the production
/// machine has none to name).
fn routes_resolve(acts: &[Act], production: bool) -> Result<(), String> {
    for act in acts {
        if let Act::Route { target } = act {
            if production {
                return Err(format!("`route {target}` names fixture packages; the production machine has no fixture to resolve it"));
            }
            let parsed = route::parse(target)?;
            route::to_route(&parsed, fixture()?).map_err(|error| short(&error))?;
        }
    }
    Ok(())
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "an index into a short list of frame times"
    )]
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn quoted(list: &[String]) -> String {
    list.iter()
        .map(|item| format!("\"{}\"", clip(item, 80)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whitespace aside: what a row reads.
fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Judges one assert; `Ok` lines pass, `Err` lines fail.
#[allow(clippy::too_many_lines, reason = "one arm per assert, each short")]
fn judge_one(runner: &Runner, seen: &Seen, image: &RgbaImage, route: Option<&Route>, assert: &Assert, production: bool) -> Vec<Result<String, String>> {
    match assert {
        Assert::Ground(tone, area) => vec![ground(seen, image, *tone, *area)],
        Assert::Route(words) => {
            let actual = route.map_or_else(|| "(no route)".to_owned(), describe);
            vec![match expected_route(words, production) {
                Ok(expected) if expected == actual => Ok(format!("route `{words}` = `{}`", short(&actual))),
                Ok(expected) => Err(format!(
                    "route: expected `{}` ({words}), actual `{}`",
                    short(&expected),
                    short(&actual)
                )),
                Err(error) => Err(format!(
                    "route `{words}` does not resolve: {}; actual `{}`",
                    short(&error),
                    short(&actual)
                )),
            }]
        }
        Assert::RouteLike(pattern) => {
            let actual = route.map_or_else(|| "(no route)".to_owned(), describe);
            vec![if script::glob(pattern, &actual) {
                Ok(format!("route like \"{pattern}\": `{}`", short(&actual)))
            } else {
                Err(format!("route like \"{pattern}\": actual `{}`", short(&actual)))
            }]
        }
        Assert::Text(list, area) => list
            .iter()
            .map(|wanted| {
                let shown = seen.texts(*area);
                match shown.iter().find(|text| &text.content == wanted) {
                    Some(text) => Ok(format!(
                        "text \"{}\"{} at ({:.0}, {:.0})",
                        clip(wanted, 80),
                        area_words(*area),
                        text.bounds.x,
                        text.bounds.y
                    )),
                    None => {
                        let elsewhere = seen.ledger.texts.iter().find(|text| &text.content == wanted).map(|text| {
                            format!(
                                "painted at ({:.0}, {:.0}), outside the {}",
                                text.bounds.x,
                                text.bounds.y,
                                area.map_or("window", Area::name)
                            )
                        });
                        let near = seen.near(wanted);
                        Err(format!(
                            "text \"{}\"{}: {}{}\n         {}",
                            clip(wanted, 80),
                            area_words(*area),
                            elsewhere.unwrap_or_else(|| "not painted".to_owned()),
                            if near.is_empty() {
                                String::new()
                            } else {
                                format!("; near: {}", near.join(", "))
                            },
                            seen.summary(*area)
                        ))
                    }
                }
            })
            .collect(),
        Assert::Like(pattern, area) => {
            let shown = seen.texts(*area);
            vec![match shown.iter().find(|text| script::glob(pattern, &text.content)) {
                Some(text) => Ok(format!("like \"{pattern}\"{}: \"{}\" at ({:.0}, {:.0})", area_words(*area), clip(&short(&text.content), 100), text.bounds.x, text.bounds.y)),
                None => Err(format!("like \"{pattern}\"{}: no visible text matches\n         {}", area_words(*area), seen.summary(*area))),
            }]
        }
        Assert::Unlike(pattern, area) => {
            let shown = seen.texts(*area);
            vec![match shown.iter().find(|text| script::glob(pattern, &text.content)) {
                None => Ok(format!("unlike \"{pattern}\"{}", area_words(*area))),
                Some(text) => Err(format!("unlike \"{pattern}\"{}: \"{}\" at ({:.0}, {:.0})", area_words(*area), clip(&short(&text.content), 100), text.bounds.x, text.bounds.y)),
            }]
        }
        Assert::Saw(list, area) => list
            .iter()
            .map(|wanted| {
                let bit = area_bit(*area);
                if runner.saw.get(wanted).is_some_and(|bits| bits & bit != 0) {
                    Ok(format!("saw \"{}\"{} since the previous checkpoint", clip(wanted, 80), area_words(*area)))
                } else {
                    let near = runner
                        .saw
                        .keys()
                        .filter(|text| {
                            let (text, wanted) = (text.to_lowercase(), wanted.to_lowercase());
                            text.contains(&wanted) || wanted.contains(&text) && text.len() >= 4
                        })
                        .take(8)
                        .map(|text| format!("\"{}\"", clip(text, 60)))
                        .collect::<Vec<_>>();
                    Err(format!(
                        "saw \"{}\"{}: never on screen since the previous checkpoint ({} distinct texts seen){}",
                        clip(wanted, 80),
                        area_words(*area),
                        runner.saw.len(),
                        if near.is_empty() { String::new() } else { format!("; near: {}", near.join(", ")) }
                    ))
                }
            })
            .collect(),
        Assert::Order(list, area) => {
            let shown = seen.texts(*area);
            let mut from = 0;
            let mut missing = None;
            for wanted in list {
                match shown[from..].iter().position(|text| &text.content == wanted) {
                    Some(offset) => from += offset + 1,
                    None => {
                        missing = Some(wanted.clone());
                        break;
                    }
                }
            }
            vec![match missing {
                None => Ok(format!("order {}{}", quoted(list), area_words(*area))),
                Some(wanted) => {
                    let found = shown
                        .iter()
                        .filter(|text| list.contains(&text.content))
                        .map(|text| format!("\"{}\"", text.content))
                        .collect::<Vec<_>>();
                    Err(format!(
                        "order {}{}: \"{wanted}\" is not on screen after the ones before it; these appear, in this order: [{}]\n         {}",
                        quoted(list),
                        area_words(*area),
                        found.join(", "),
                        seen.summary(*area)
                    ))
                }
            }]
        }
        Assert::Absent(list, area) => list
            .iter()
            .map(|unwanted| match seen.texts(*area).iter().find(|text| &text.content == unwanted) {
                None => Ok(format!("absent \"{}\"{}", clip(unwanted, 80), area_words(*area))),
                Some(text) => Err(format!(
                    "absent \"{}\"{}: on screen at ({:.0}, {:.0})",
                    clip(unwanted, 80),
                    area_words(*area),
                    text.bounds.x,
                    text.bounds.y
                )),
            })
            .collect(),
        Assert::Line(list, area) => {
            let rows = seen.lines(*area);
            list.iter()
                .map(|wanted| {
                    let needle = squash(wanted);
                    match rows.iter().find(|row| squash(row).contains(&needle)) {
                        Some(row) => Ok(format!(
                            "line \"{wanted}\"{}: \"{}\"",
                            area_words(*area),
                            clip(&short(row), 120)
                        )),
                        None => {
                            let first = wanted
                                .split_whitespace()
                                .next()
                                .unwrap_or_default()
                                .trim_end_matches(',');
                            let related = rows
                                .iter()
                                .filter(|row| !first.is_empty() && row.contains(first))
                                .take(4)
                                .map(|row| format!("\"{}\"", clip(&short(row), 120)))
                                .collect::<Vec<_>>();
                            Err(format!(
                                "line \"{wanted}\"{}: no row reads it; {}",
                                area_words(*area),
                                if related.is_empty() {
                                    format!(
                                        "rows: [{}]",
                                        rows.iter()
                                            .take(24)
                                            .map(|row| format!("\"{}\"", clip(&short(row), 80)))
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    )
                                } else {
                                    format!("rows with \"{first}\": [{}]", related.join(", "))
                                }
                            ))
                        }
                    }
                })
                .collect()
        }
        Assert::Link(pick) => vec![
            seen.locate(pick)
                .map(|(_, what, _)| format!("link {pick}: {what}"))
                .map_err(|why| format!("link {pick}: {why}")),
        ],
        Assert::State { key, glob } => {
            let value = seen.state.iter().find(|(name, _)| name == key).map(|(_, value)| value.as_str());
            vec![match value {
                Some(value) if script::glob(glob, value) => Ok(format!("state {key} \"{glob}\": `{}`", clip(value, 100))),
                Some(value) => Err(format!("state {key} \"{glob}\": `{}`", clip(value, 100))),
                None => Err(format!("state {key}: the app does not say")),
            }]
        }
        Assert::Size { text, at_least, area } => {
            let shown = seen.texts(*area);
            vec![match shown.iter().filter(|shown| &shown.content == text).map(|shown| shown.line_height).reduce(f32::max) {
                Some(tall) if tall >= *at_least => Ok(format!("size \"{}\"{}: {tall:.1} px tall (at least {at_least:.1})", clip(text, 80), area_words(*area))),
                Some(tall) => Err(format!("size \"{}\"{}: {tall:.1} px tall, not at least {at_least:.1}", clip(text, 80), area_words(*area))),
                None => Err(format!("size \"{}\"{}: not on screen\n         {}", clip(text, 80), area_words(*area), seen.summary(*area))),
            }]
        }
        Assert::FocusRestored => {
            let here = route.map_or_else(|| "(no route)".to_owned(), describe);
            let focused = seen.focused();
            let names = focused.iter().map(|target| format!("`{}`", short(&target.key))).collect::<Vec<_>>().join(", ");
            vec![match runner.left_by.get(&here) {
                None => Err(format!("focus restored: this route (`{}`) was never left, so nothing can come back", short(&here))),
                Some(Leave { key: None, how, .. }) => Err(format!(
                    "focus restored: this route was left by `{how}`, not by a click on a target, so there is no focus to restore; focused: [{names}]"
                )),
                Some(Leave { key: Some(key), bounds, how }) => match focused.as_slice() {
                    [target] if &target.key == key => Ok(format!("focus restored: `{}` (left by `{how}`)", short(key))),
                    [target] if bounds.as_ref().is_some_and(|clicked| same_box(clicked, &target.bounds)) => Ok(format!(
                        "focus restored: `{}`, the door over `{}` (left by `{how}`)",
                        short(&target.key),
                        short(key)
                    )),
                    _ => Err(format!(
                        "focus restored: left by `{how}` on `{}`, but focused: [{names}]",
                        short(key)
                    )),
                },
            }]
        }
        Assert::Focus(pick) => vec![
            seen.focus_is(pick)
                .map(|evidence| format!("focus {pick}: {evidence}"))
                .map_err(|why| format!("focus {pick}: {why}")),
        ],
        Assert::Budget { what, limit_ms } => {
            let (measured, detail) = match what {
                Budget::PageOpen | Budget::Search => {
                    let anchor = runner.anchor.as_ref();
                    (
                        anchor.and_then(|anchor| anchor.open_ms),
                        anchor.map_or_else(String::new, |anchor| format!(" after `{}`", anchor.label)),
                    )
                }
                Budget::Flight => {
                    let anchor = runner.anchor.as_ref();
                    #[allow(clippy::cast_precision_loss, reason = "virtual ms stay far below 2^52")]
                    (
                        anchor.and_then(|anchor| anchor.settled_ms).map(|ms| ms as f64),
                        anchor.map_or_else(String::new, |anchor| format!(" after `{}`", anchor.label)),
                    )
                }
                Budget::FrameP95 => (
                    Some(percentile(&runner.cpu_ms, 0.95)),
                    format!(" over {} frames", runner.cpu_ms.len()),
                ),
            };
            vec![match measured {
                Some(ms) if !JUDGE_BUDGETS => Ok(format!(
                    "budget {} {ms:.1} ms (limit {limit_ms} ms; not judged in a debug build){detail}",
                    what.name()
                )),
                Some(ms) if ms <= *limit_ms => {
                    Ok(format!("budget {} {ms:.1} ms <= {limit_ms} ms{detail}", what.name()))
                }
                Some(ms) => Err(format!("budget {} {ms:.1} ms > {limit_ms} ms{detail}", what.name())),
                None => Err(format!("budget {}: not measured{detail}", what.name())),
            }]
        }
    }
}

/// The mean relative luminance of `area`'s pixels in the captured frame,
/// judged dark (< 0.35) or light (> 0.65).
fn ground(seen: &Seen, image: &RgbaImage, tone: Tone, area: Option<Area>) -> Result<String, String> {
    let viewport = seen.drawn.viewport;
    let (left, top, right, bottom) = area.map_or_else(
        || {
            #[allow(clippy::cast_precision_loss, reason = "window sizes are small")]
            (0.0, 0.0, viewport.width as f32, viewport.height as f32)
        },
        |area| look::rect(area, seen.frame.as_ref(), viewport),
    );
    #[allow(clippy::cast_precision_loss, reason = "window sizes are small")]
    let scale = image.width() as f32 / viewport.width.max(1) as f32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to the image")]
    let px = |value: f32, limit: u32| ((value * scale).max(0.0) as u32).min(limit);
    let (x0, x1, y0, y1) = (px(left, image.width()), px(right, image.width()), px(top, image.height()), px(bottom, image.height()));
    let channel = |value: u8| {
        let value = f64::from(value) / 255.0;
        if value <= 0.040_45 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    };
    let (mut sum, mut count) = (0.0_f64, 0_u32);
    for y in (y0..y1).step_by(3) {
        for x in (x0..x1).step_by(3) {
            let [r, g, b, _] = image.get_pixel(x, y).0;
            sum += 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
            count += 1;
        }
    }
    if count == 0 {
        return Err(format!("ground{}: the area has no pixels", area_words(area)));
    }
    let luma = sum / f64::from(count);
    let (held, words) = match tone {
        Tone::Dark => (luma < 0.35, "dark"),
        Tone::Light => (luma > 0.65, "light"),
    };
    let evidence = format!("ground {words}{}: mean luminance {luma:.3} over {count} pixels of {}x{}", area_words(area), x1 - x0, y1 - y0);
    if held { Ok(evidence) } else { Err(evidence) }
}

/// Judges the settled frame against `asserts`, plus zero lints.
fn judge(
    runner: &mut Runner,
    step: &Step,
    name: &str,
    asserts: &[Assert],
    image: &RgbaImage,
    unsettled: Option<String>,
    production: bool,
) -> Result<Judged, String> {
    let route = runner.route()?;
    let seen = runner.seen()?;
    let mut lines = Vec::new();
    let mut content = true;
    if let Some(why) = unsettled {
        content = false;
        lines.push(format!("  FAIL settle: {why}"));
    }
    for assert in asserts {
        for outcome in judge_one(runner, seen, image, route.as_ref(), assert, production) {
            match outcome {
                Ok(evidence) => lines.push(format!("  ok   {evidence}")),
                Err(evidence) => {
                    content = false;
                    lines.push(format!("  FAIL {evidence}"));
                }
            }
        }
    }
    let mut passed = content;
    // Words under a dialog's opaque plate are not on screen: not linted.
    let mut linted = lint::lint(image, &look::unoccluded(&seen.ledger), seen.drawn.viewport);
    // While a dialog is up, the page under its scrim is not content: its
    // dimmed words are not held to text contrast (every other rule holds).
    let dialogs = seen
        .ledger
        .stacks
        .iter()
        .flat_map(|stack| &stack.entries)
        .filter(|entry| entry.kind == "dialog")
        .filter_map(|entry| entry.bounds.clone())
        .collect::<Vec<_>>();
    let under_scrim = |key: &str| {
        !dialogs.is_empty()
            && seen.ledger.texts.iter().find(|text| text.key == key).is_some_and(|text| {
                let (x, y) = (text.bounds.x + text.bounds.width / 2.0, text.bounds.y + text.bounds.height / 2.0);
                !dialogs.iter().any(|plate| x >= plate.x && x <= plate.x + plate.width && y >= plate.y && y <= plate.y + plate.height)
            })
    };
    let before = linted.lints.len();
    linted.lints.retain(|item| !(item.rule.name() == "contrast" && under_scrim(&item.key)));
    if linted.lints.len() < before {
        lines.push(format!("  note {} contrast lints on the page under the dialog's scrim, not held (dialog plate {:?})", before - linted.lints.len(), dialogs.iter().map(|b| (b.x.round(), b.y.round(), b.width.round(), b.height.round())).collect::<Vec<_>>()));
    }
    if linted.lints.is_empty() {
        lines.push(format!(
            "  ok   lints: none ({} texts, {} targets linted)",
            linted.coverage.texts, linted.coverage.targets
        ));
    } else {
        passed = false;
        lines.push(format!(
            "  FAIL lints: {} ({} texts, {} targets linted)",
            linted.lints.len(),
            linted.coverage.texts,
            linted.coverage.targets
        ));
        for item in linted.lints.iter().take(12) {
            lines.push(format!(
                "         {} {}: {}",
                item.rule.name(),
                short(&item.key),
                short(&item.detail)
            ));
        }
        if linted.lints.len() > 12 {
            lines.push(format!("         … {} more", linted.lints.len() - 12));
        }
    }
    Ok(Judged {
        name: name.to_owned(),
        origin: step.origin.to_string(),
        at_ms: seen.drawn.at_ms,
        passed,
        content,
        needs: step.needs.clone(),
        lines,
    })
}

fn slug(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect()
}

fn save(image: &RgbaImage, path: &Path) -> Result<(), String> {
    image
        .save(path)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Two published targets that frame one thing: every edge within 2 px.
fn same_box(a: &facet::probe::BoundsSample, b: &facet::probe::BoundsSample) -> bool {
    [(a.x, b.x), (a.y, b.y), (a.x + a.width, b.x + b.width), (a.y + a.height, b.y + b.height)].iter().all(|(p, q)| (p - q).abs() <= 2.0)
}

/// How a route was left: the target clicked (if any), its box, and the step.
struct Leave {
    key: Option<String>,
    /// The clicked target's box: a control and the keyboard door laid over it
    /// are published as two targets of one box (a module's card is facet's
    /// control and the page's door), and focus on either is focus on it.
    bounds: Option<facet::probe::BoundsSample>,
    how: String,
}

/// Does one acts, pointer or picker step. `Ok(Err(why))`: the person could not.
fn perform(runner: &mut Runner, kind: &StepKind, label: &str, origin: &str, report: &mut String, production: bool) -> Result<Result<(), String>, String> {
    let before = runner.route_words()?;
    let (acts, key, what) = match kind {
        StepKind::Acts(acts) => {
            // A route the index cannot resolve fails the step; it never
            // reaches the product adapter (which would panic).
            if let Err(why) = routes_resolve(acts, production) {
                return Ok(Err(why));
            }
            (acts.clone(), None, String::new())
        }
        StepKind::Pointer { click, pick } => {
            if let Err(why) = runner.settle()? {
                let _ = writeln!(report, "  {origin:<28} note: not still before `{label}`: {why}");
            }
            match runner.seen()?.locate(pick) {
                Ok(((x, y), what, key)) => {
                    let act = if *click {
                        Act::Click {
                            x,
                            y,
                            button: backend_gui_harness::Button::Left,
                        }
                    } else {
                        Act::Move { x, y }
                    };
                    (vec![act], click.then_some(key), format!("  ({what})"))
                }
                Err(why) => return Ok(Err(why)),
            }
        }
        StepKind::AnswerPicker(answer) => {
            let chosen = match answer {
                PickerAnswer::Folders(folders) => Some(folders.clone()),
                PickerAnswer::Cancel => None,
            };
            if !machine::answer_picker(runner.session()?, chosen)? {
                return Ok(Err("no native folder panel is open: nothing the product did asked for one".to_owned()));
            }
            runner.performed.push(Performed::Answer(answer.clone()));
            runner.tick(false)?;
            let _ = writeln!(report, "  {origin:<28} {:>6} ms  {label}  (the native panel, answered)", runner.now());
            return Ok(Ok(()));
        }
        StepKind::Settle | StepKind::Wait(_) | StepKind::Check { .. } | StepKind::Await { .. } | StepKind::Restart | StepKind::Crawl(_) => {
            return Err(format!("{origin}: `{label}` is not an act"));
        }
    };
    let bounds = match &key {
        Some(key) => runner.seen()?.ledger.targets.iter().find(|target| &target.key == key).map(|target| target.bounds.clone()),
        None => None,
    };
    runner.deliver(&acts, label)?;
    let words = acts.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    let _ = writeln!(
        report,
        "  {origin:<28} {:>6} ms  {label}{}{what}",
        runner.now(),
        if matches!(kind, StepKind::Pointer { .. }) { format!("  ->  {words}") } else { String::new() }
    );
    let after = runner.route_words()?;
    if after != before {
        runner.left_by.insert(
            before,
            Leave {
                key,
                bounds,
                how: label.to_owned(),
            },
        );
    }
    Ok(Ok(()))
}

/// How the run ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Every step, checkpoint, motion and settle == fresh held.
    Pass,
    /// Something that should work did not.
    Fail(String),
    /// A gated step failed: the product or its data lacks what it needs.
    Blocked(Vec<String>),
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pass => f.write_str("PASS"),
            Self::Fail(why) => write!(f, "FAIL ({why})"),
            Self::Blocked(gaps) => write!(f, "BLOCKED({})", gaps.join("; ")),
        }
    }
}

/// What a finished run printed and how it ended.
pub struct Outcome {
    /// The REPORT.
    pub report: String,
    /// The verdict.
    pub verdict: Verdict,
    /// Every step was done and every checkpoint's content held (lints,
    /// motion and settle == fresh aside): the machine really reached the
    /// state the plan describes. A state is kept on this, and its lints are
    /// judged by the journeys that start from it.
    pub content: bool,
}

/// How a run is asked for.
#[derive(Clone, Debug)]
pub struct Options {
    /// The capture scale (1 or 2).
    pub scale: u8,
    /// Whether to run settle == fresh.
    pub fresh: bool,
    /// Materialize every state again even when its key is ready.
    pub remake_states: bool,
    /// Run a production journey on the fixture machine instead (the proof
    /// that a journey refuses pre-admitted roots).
    pub force_fixture: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { scale: 2, fresh: true, remake_states: false, force_fixture: false }
    }
}

fn uptime() -> String {
    std::process::Command::new("uptime")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_else(|| "uptime unavailable".to_owned())
}

/// Opens the machine `plan` starts on. `Ok(Err(words))`: a state it starts
/// from could not be materialized (the journey is BLOCKED on it).
fn start_machine(plan: &Plan, options: &Options, report: &mut String) -> Result<Result<Runner, String>, String> {
    if options.force_fixture || !plan.start.is_production() {
        let route = match &plan.start {
            Start::Fixture { route } => route.clone(),
            Start::Clean | Start::From(_) => "orbit".to_owned(),
        };
        let started = Instant::now();
        let session = open_fixture(&route, plan.size, options.scale)?;
        let _ = writeln!(
            report,
            "machine: FIXTURE (the harness's index, roots pre-admitted: not an install){}, boot `{route}` in {:.1} s wall",
            if options.force_fixture && plan.start.is_production() { ", forced by --machine fixture" } else { "" },
            started.elapsed().as_secs_f64()
        );
        return Ok(Ok(Runner::new(session, None, None, plan.size, options.scale, true)));
    }
    let store = Store::lane();
    match &plan.start {
        Start::Clean => {
            store.wipe_live()?;
            let _ = writeln!(report, "machine: PRODUCTION, clean (an empty user root at {}; no state dir, no settings, no projects)", short(&store.live.display().to_string()));
        }
        Start::From(recipe) => {
            let key = match materialize(recipe, plan.size, options, &store, report)? {
                Ok(key) => key,
                Err(blocked) => return Ok(Err(blocked)),
            };
            store.restore(&key)?;
            let _ = writeln!(report, "machine: PRODUCTION from state {recipe} (key {}), cloned to {}", key.short(), short(&store.live.display().to_string()));
        }
        Start::Fixture { .. } => unreachable!("handled above"),
    }
    let started = Instant::now();
    let (session, launched) = machine::open_production(&store.live, plan.size, options.scale)?;
    let _ = writeln!(report, "launch: {:.1} s wall to the first quiet frame (the owner answered)", started.elapsed().as_secs_f64());
    Ok(Ok(Runner::new(session, Some(launched), Some(store.live.clone()), plan.size, options.scale, true)))
}

/// The state `recipe` leaves, by key: made now by running its plan from
/// clean when the key is not ready. `Ok(Err(words))`: its run did not pass.
fn materialize(recipe: &Recipe, size: (u32, u32), options: &Options, store: &Store, report: &mut String) -> Result<Result<StateKey, String>, String> {
    let parts = Parts::load(&Parts::dir())?;
    let state_plan = recipe.plan(&parts, size)?;
    let mut inputs = Vec::new();
    for input in recipe.inputs() {
        let hash = state::input_hash(&input)?;
        inputs.push((input, hash));
    }
    let key = state::key(&state_plan, &inputs, &state::build_id()?, &state::toolchain());
    let dir = store.state(&key);
    if store.ready(&key) && !options.remake_states {
        let _ = writeln!(report, "state: {recipe}: key {} ready (made earlier; evidence {})", key.short(), short(&dir.join("run/REPORT.txt").display().to_string()));
        return Ok(Ok(key));
    }
    let evidence = store.evidence(&key)?;
    let _ = writeln!(report, "state: {recipe}: key {} not ready; materializing it now by running its plan from clean", key.short());
    eprintln!("[journey] materializing state {recipe} (key {}) from clean; evidence {}", key.short(), evidence.display());
    let started = Instant::now();
    let outcome = run(&state_plan, &evidence, &Options { fresh: false, ..options.clone() })?;
    let _ = writeln!(report, "state: {recipe}: its run took {:.0} s wall and says {}", started.elapsed().as_secs_f64(), outcome.verdict);
    if !outcome.content {
        return Ok(Err(format!("state: {recipe} did not materialize: {} (see {})", outcome.verdict, short(&evidence.join("REPORT.txt").display().to_string()))));
    }
    if outcome.verdict != Verdict::Pass {
        let _ = writeln!(report, "state: {recipe}: every step and content check held, so the state is real and kept; its other findings are in its REPORT");
    }
    store.keep(&key)?;
    let _ = writeln!(report, "state: {recipe}: kept as {}", short(&dir.display().to_string()));
    Ok(Ok(key))
}

/// Runs `plan` and writes its evidence to `out`.
///
/// # Errors
/// The harness itself failing (a window, a write); a failing journey is an
/// [`Outcome`], not an error.
#[allow(clippy::too_many_lines, reason = "the run in order: machine, steps, end, motion, fresh, strip, verdict")]
pub fn run(plan: &Plan, out: &Path, options: &Options) -> Result<Outcome, String> {
    std::fs::create_dir_all(out).map_err(|error| format!("{}: {error}", out.display()))?;
    let production = plan.start.is_production() && !options.force_fixture;
    let started = Instant::now();
    let mut report = String::new();
    let source = short(&plan.source.display().to_string());
    let _ = writeln!(
        report,
        "journey {}  {}x{}@{}x  script: {source}\nbuild: {}\nwall at start: {}",
        plan.name,
        plan.size.0,
        plan.size.1,
        options.scale,
        if JUDGE_BUDGETS { "release" } else { "debug (budgets measured, not judged)" },
        uptime()
    );
    let mut runner = match start_machine(plan, options, &mut report)? {
        Ok(runner) => runner,
        Err(blocked) => {
            let verdict = Verdict::Blocked(vec![blocked]);
            let _ = writeln!(report, "\nverdict: {verdict}");
            std::fs::write(out.join("REPORT.txt"), &report).map_err(err)?;
            return Ok(Outcome { report, verdict, content: false });
        }
    };
    let mut inventories = String::new();
    let mut failed: Vec<String> = Vec::new();
    let mut blocked: Vec<String> = Vec::new();
    // Checkpoints whose content held and whose lints did not.
    let mut lint_only: Vec<String> = Vec::new();
    let mut stopped = None;
    let mut judged: Vec<Judged> = Vec::new();
    runner.film = Some(Film {
        label: "launch".to_owned(),
        at_ms: 0,
        next: 0,
    });
    if let Err(why) = runner.settle()? {
        failed.push("launch never settled".to_owned());
        let _ = writeln!(report, "FAIL launch never settled: {why}");
    }
    let _ = writeln!(report, "\nsteps:");
    for step in &plan.steps {
        if let Some(at) = &stopped {
            let _ = writeln!(report, "  {:<28} not run: blocked at {at}", step.origin.to_string());
            continue;
        }
        let origin = step.origin.to_string();
        eprintln!("[journey] {} {origin}  {}", plan.name, step.text);
        let outcome: Result<(), String> = match &step.kind {
            StepKind::Acts(_) | StepKind::Pointer { .. } | StepKind::AnswerPicker(_) => {
                match perform(&mut runner, &step.kind, &step.text, &origin, &mut report, production)? {
                    Ok(()) => Ok(()),
                    Err(why) => {
                        if let Some(detour) = &step.otherwise {
                            let label = format!("DETOUR (the person could not do {origin}) {}", step.text.split(" else ").nth(1).unwrap_or_default());
                            if let Err(detour_why) = perform(&mut runner, detour, &label, &origin, &mut report, production)? {
                                let _ = writeln!(report, "  {origin:<28} FAIL detour: {detour_why}");
                            }
                        }
                        Err(why)
                    }
                }
            }
            StepKind::Settle => match runner.settle()? {
                Ok(_) => {
                    let _ = writeln!(report, "  {origin:<28} {:>6} ms  settled", runner.now());
                    Ok(())
                }
                Err(why) => Err(format!("settle: {why}")),
            },
            StepKind::Wait(ms) => {
                let until = runner.now() + ms;
                while runner.now() < until {
                    runner.tick(false)?;
                }
                let _ = writeln!(report, "  {origin:<28} {:>6} ms  waited {ms} ms", runner.now());
                Ok(())
            }
            StepKind::Await { until, within } => {
                let _ = writeln!(report, "  {origin:<28} {:>6} ms  {} (real time; new texts as they appeared:)", runner.now(), step.text);
                runner.await_until(until, *within, &mut report)?
            }
            StepKind::Crawl(crawl) => {
                let _ = writeln!(report, "  {origin:<28} {:>6} ms  {} (every target: hover, click, back)", runner.now(), step.text);
                let crawled = runner.crawl(*crawl, &mut report)?;
                let _ = writeln!(
                    report,
                    "  {origin:<28} {:>6} ms  crawled {} page(s): {} hover(s), {} card(s), {} click(s); {} problem(s)",
                    runner.now(),
                    crawled.pages,
                    crawled.hovers,
                    crawled.cards,
                    crawled.clicks,
                    crawled.problems.len()
                );
                for problem in crawled.problems.iter().take(40) {
                    let _ = writeln!(report, "           FAIL {problem}");
                }
                if crawled.problems.len() > 40 {
                    let _ = writeln!(report, "           … {} more", crawled.problems.len() - 40);
                }
                if crawled.problems.is_empty() { Ok(()) } else { Err(format!("crawl: {} problem(s), first: {}", crawled.problems.len(), crawled.problems[0])) }
            }
            StepKind::Restart => {
                let wall = Instant::now();
                if runner.live.is_none() {
                    Err("`restart` needs the production machine; this one is the fixture".to_owned())
                } else {
                    runner.restart()?;
                    let unsettled = runner.settle()?.err();
                    let _ = writeln!(report, "  {origin:<28} {:>6} ms  restart: quit, relaunched in {:.1} s wall", runner.now(), wall.elapsed().as_secs_f64());
                    unsettled.map_or(Ok(()), |why| Err(format!("the relaunch never settled: {why}")))
                }
            }
            StepKind::Check { name, asserts } => {
                let unsettled = runner.settle()?.err();
                let image = runner
                    .tick(true)?
                    .ok_or_else(|| "the checkpoint frame was not captured".to_owned())?;
                let result = judge(&mut runner, step, name, asserts, &image, unsettled, production)?;
                let file = format!("{:02}-{}.png", judged.len() + 1, slug(name));
                save(&image, &out.join(&file))?;
                runner.tiles.push(Tile {
                    label: format!(
                        "{} check {name}  t={} ms",
                        if result.passed { "PASS" } else { "FAIL" },
                        result.at_ms
                    ),
                    image: tile(&image),
                });
                let route = runner.route()?;
                let _ = writeln!(inventories, "check {name} ({origin}) t={} ms  frame {file}", result.at_ms);
                inventories.push_str(&runner.seen()?.inventory(route.as_ref()));
                inventories.push('\n');
                let _ = writeln!(
                    report,
                    "  {origin:<28} {:>6} ms  check {name}: {}",
                    result.at_ms,
                    if result.passed { "PASS" } else { "FAIL" }
                );
                runner.saw.clear();
                let (passed, content) = (result.passed, result.content);
                judged.push(result);
                if passed {
                    Ok(())
                } else if content {
                    lint_only.push(format!("{origin} check {name}: lints"));
                    Ok(())
                } else {
                    Err(format!("check {name}"))
                }
            }
        };
        match (outcome, &step.needs) {
            (Ok(()), Some(gap)) => {
                let _ = writeln!(report, "  {origin:<28} note: gated on {} \"{}\", and it held: the gap may be closed (GAPS.md)", gap.kind.name(), gap.what);
            }
            (Ok(()), None) => {}
            (Err(why), Some(gap)) => {
                blocked.push(format!("{}: {}", gap.kind.name(), gap.what));
                let _ = writeln!(report, "  {origin:<28} BLOCKED({}: {}): {}", gap.kind.name(), gap.what, first_line(&why));
                // A gated act the person could not do leaves nothing to walk
                // on; a gated check changed nothing, so the walk goes on.
                if step.otherwise.is_none() && !matches!(step.kind, StepKind::Check { .. }) {
                    stopped = Some(origin.clone());
                }
            }
            (Err(why), None) => {
                if !matches!(step.kind, StepKind::Check { .. }) {
                    let _ = writeln!(report, "  {origin:<28} FAIL {}: {}", step.text, why);
                }
                failed.push(format!("{origin} {}", first_line(&why)));
            }
        }
    }
    // The end: the pointer leaves, everything settles.
    runner.deliver(&[Act::Leave], "leave (end)")?;
    let end_unsettled = runner.settle()?.err();
    let end = runner
        .tick(true)?
        .ok_or_else(|| "the end frame was not captured".to_owned())?;
    save(&end, &out.join("end-settled.png"))?;
    runner.tiles.push(Tile {
        label: format!("end, settled  t={} ms", runner.now()),
        image: tile(&end),
    });
    let _ = writeln!(report, "\ncheckpoints:");
    for result in &judged {
        let _ = writeln!(
            report,
            "check {} ({}) at {} ms: {}{}",
            result.name,
            result.origin,
            result.at_ms,
            if result.passed { "PASS" } else { "FAIL" },
            result.needs.as_ref().map_or_else(String::new, |gap| format!("  (gated: {} \"{}\")", gap.kind.name(), gap.what))
        );
        for line in &result.lines {
            let _ = writeln!(report, "{line}");
        }
    }
    // Motion over the whole journey.
    let alignment = align::analyze(&runner.observed, Tolerance::default());
    let _ = writeln!(report, "\nmotion ({} frames, {} ms):", alignment.frames, runner.now());
    for line in align::text(&plan.name, &alignment, 24).lines() {
        let _ = writeln!(report, "  {line}");
    }
    write_motion(plan, &runner, &judged, &alignment, out)?;
    let _ = writeln!(
        report,
        "\nframes: {} drawn, draw p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        runner.cpu_ms.len(),
        percentile(&runner.cpu_ms, 0.5),
        percentile(&runner.cpu_ms, 0.95),
        percentile(&runner.cpu_ms, 1.0)
    );
    let replayed = Script { events: runner.events.clone() };
    std::fs::write(out.join("resolved.txt"), replayed.to_string()).map_err(err)?;
    let tiles = std::mem::take(&mut runner.tiles);
    let performed = runner.performed.clone();
    // The window and the owner go before a calm replay reuses the machine.
    if let (Some(session), Some(launched)) = (runner.session.take(), runner.launched.take()) {
        machine::quit(session, launched);
    }
    drop(runner);
    // Settle == fresh.
    let mut fresh_verdict = if end_unsettled.is_some() { "FAIL" } else { "not run" };
    if let Some(why) = &end_unsettled {
        let _ = writeln!(report, "end: FAIL never settled: {why}");
    }
    if stopped.is_some() {
        let _ = writeln!(report, "settle == fresh: not run (the journey stopped where it was blocked)");
    } else if options.fresh {
        match calm_replay(plan, &performed, options) {
            Ok((image, notes)) => {
                for note in &notes {
                    let _ = writeln!(report, "fresh replay note: {note}");
                }
                match gallery::storm::difference(&end, &image) {
                    None => {
                        if fresh_verdict != "FAIL" {
                            fresh_verdict = "ok";
                        }
                        let _ = writeln!(report, "settle == fresh: identical ({} steps replayed calmly on a fresh launch)", performed.len());
                    }
                    Some(detail) => {
                        fresh_verdict = "FAIL";
                        save(&image, &out.join("end-fresh.png"))?;
                        let _ = writeln!(
                            report,
                            "settle == fresh: FAIL, {detail} (end-settled.png vs end-fresh.png; {} steps replayed calmly)",
                            performed.len()
                        );
                    }
                }
            }
            Err(error) => {
                fresh_verdict = "FAIL";
                let _ = writeln!(report, "settle == fresh: FAIL, the calm replay failed: {error}");
            }
        }
    } else {
        let _ = writeln!(report, "settle == fresh: not run (--no-fresh)");
    }
    // The strip.
    let lines = tiles.iter().map(|tile| tile.label.clone()).collect::<Vec<_>>();
    let labels = compose::labels(&lines, TILE_WIDTH / 2, 2, facet::Appearance::Abyss).map_err(err)?;
    let images = tiles.iter().map(|tile| &tile.image).collect::<Vec<_>>();
    let strip = compose::sheet(&images, &labels, 4, TILE_WIDTH, compose::background(facet::Appearance::Abyss));
    save(&strip, &out.join("strip.png"))?;
    std::fs::write(out.join("checks.txt"), inventories).map_err(err)?;
    // The verdict. `content`: the steps happened and the checkpoints' words
    // held, whatever the lints, the motion and settle == fresh say.
    let content = failed.is_empty() && blocked.is_empty() && stopped.is_none() && end_unsettled.is_none();
    if stopped.is_none() {
        if !alignment.passed() {
            failed.push(format!("motion: {} findings", alignment.findings.len()));
        }
        if options.fresh && fresh_verdict != "ok" {
            failed.push("settle == fresh".to_owned());
        }
    }
    failed.extend(lint_only);
    let verdict = if !failed.is_empty() {
        Verdict::Fail(failed.join("; "))
    } else if !blocked.is_empty() {
        Verdict::Blocked(blocked)
    } else {
        Verdict::Pass
    };
    let failed_checks = judged.iter().filter(|result| !result.passed).count();
    let _ = writeln!(
        report,
        "\nverdict: {verdict}\n  {} of {} checkpoints pass; motion {}; settle == fresh {fresh_verdict}; strip: {} tiles in strip.png\nwall: {:.0} s; at end: {}",
        judged.len() - failed_checks,
        judged.len(),
        if alignment.passed() { "clean".to_owned() } else { format!("{} findings", alignment.findings.len()) },
        tiles.len(),
        started.elapsed().as_secs_f64(),
        uptime()
    );
    std::fs::write(out.join("REPORT.txt"), &report).map_err(err)?;
    Ok(Outcome { report, verdict, content })
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

fn write_motion(plan: &Plan, runner: &Runner, judged: &[Judged], alignment: &align::Alignment, out: &Path) -> Result<(), String> {
    let motion = Json::Obj(vec![
        ("journey".to_owned(), Json::str(plan.name.clone())),
        ("alignment".to_owned(), align::json(&plan.name, alignment)),
        (
            "acts".to_owned(),
            Json::Arr(
                runner
                    .events
                    .iter()
                    .map(|event| {
                        Json::obj([
                            ("at_ms", Json::num(u32::try_from(event.at_ms).unwrap_or(u32::MAX))),
                            ("act", Json::str(event.act.to_string())),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "checkpoints".to_owned(),
            Json::Arr(
                judged
                    .iter()
                    .map(|result| {
                        Json::obj([
                            ("name", Json::str(result.name.clone())),
                            ("at_ms", Json::num(u32::try_from(result.at_ms).unwrap_or(u32::MAX))),
                            ("pass", Json::Bool(result.passed)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ]);
    std::fs::write(out.join("motion.json"), motion.to_string()).map_err(err)
}

/// Prepares the start machine again and replays what the run did, each
/// step settled before the next: the frame the journey must equal.
fn calm_replay(plan: &Plan, performed: &[Performed], options: &Options) -> Result<(RgbaImage, Vec<String>), String> {
    let mut scratch = String::new();
    let mut runner = start_machine(plan, &Options { fresh: false, remake_states: false, ..options.clone() }, &mut scratch)?
        .map_err(|blocked| format!("the start machine is not available: {blocked}"))?;
    runner.filming = false;
    let mut notes = Vec::new();
    if let Err(why) = runner.settle()? {
        notes.push(format!("fresh launch: {why}"));
    }
    for step in performed {
        match step {
            Performed::Acts(acts) => {
                for act in acts {
                    runner.deliver(std::slice::from_ref(act), &act.to_string())?;
                    if let Err(why) = runner.settle()? {
                        notes.push(format!("after `{act}`: {why}"));
                    }
                }
            }
            Performed::Answer(answer) => {
                let chosen = match answer {
                    PickerAnswer::Folders(folders) => Some(folders.clone()),
                    PickerAnswer::Cancel => None,
                };
                if !machine::answer_picker(runner.session()?, chosen)? {
                    notes.push("the calm replay found no folder panel to answer".to_owned());
                }
                if let Err(why) = runner.settle()? {
                    notes.push(format!("after the panel's answer: {why}"));
                }
            }
            Performed::Await { until, within } => {
                let mut ignored = String::new();
                if let Err(why) = runner.await_until(until, *within, &mut ignored)? {
                    notes.push(format!("await: {why}"));
                }
            }
            Performed::Restart => {
                runner.restart()?;
                if let Err(why) = runner.settle()? {
                    notes.push(format!("after the relaunch: {why}"));
                }
            }
        }
    }
    let image = runner
        .tick(true)?
        .ok_or_else(|| "the calm replay's last frame was not captured".to_owned())?;
    if let (Some(session), Some(launched)) = (runner.session.take(), runner.launched.take()) {
        machine::quit(session, launched);
    }
    Ok((image, notes))
}

fn usage() -> String {
    "usage: backend-desktop-gui-harness journey NAME|FILE [--out DIR] [--scale 1|2] [--no-fresh] [--index DIR] [--remake-states] [--machine fixture]"
        .to_owned()
}

/// The journey command line.
#[must_use]
pub fn main(args: &[String]) -> ExitCode {
    match command(args) {
        Ok(Verdict::Pass) => ExitCode::SUCCESS,
        Ok(Verdict::Fail(_)) => ExitCode::FAILURE,
        Ok(Verdict::Blocked(_)) => ExitCode::from(3),
        Err(error) => {
            eprintln!("journey: {error}");
            ExitCode::from(2)
        }
    }
}

fn command(args: &[String]) -> Result<Verdict, String> {
    let mut name = None;
    let mut out = None;
    let mut options = Options::default();
    let mut index = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(iter.next().ok_or_else(usage)?)),
            "--index" => index = Some(PathBuf::from(iter.next().ok_or_else(usage)?)),
            "--scale" => {
                options.scale = iter
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|scale| matches!(scale, 1 | 2))
                    .ok_or_else(usage)?;
            }
            "--no-fresh" => options.fresh = false,
            "--remake-states" => options.remake_states = true,
            "--machine" => match iter.next().map(String::as_str) {
                Some("fixture") => options.force_fixture = true,
                _ => return Err(usage()),
            },
            other if other.starts_with("--") || name.is_some() => return Err(usage()),
            other => name = Some(other.to_owned()),
        }
    }
    let name = name.ok_or_else(usage)?;
    let parts = Parts::load(&Parts::dir())?;
    // J11 is built from the key table, not read from a file.
    let (plan, stem) = if name == "J11" {
        (keys::plan(&parts)?, name.clone())
    } else {
        let path = if Path::new(&name).is_file() {
            PathBuf::from(&name)
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("journeys").join(format!("{name}.journey"))
        };
        let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(&name)
            .to_owned();
        (Plan::parse(&stem, &path, &text, &parts)?, stem)
    };
    // Journeys keep their own copy of the fixture index (same roots): a
    // journey runs for minutes and would hold the scenes' index lock.
    super::keep_index_in(
        index
            .or_else(|| std::env::var_os("NUDOX_HARNESS_STATE").map(|state| PathBuf::from(state).join("index")))
            .unwrap_or_else(|| super::repo().join(".local/harness/journeys/index")),
    );
    let out = out.unwrap_or_else(|| super::repo().join(".local/harness/journeys").join(&stem));
    // The live machine is this process's for the whole run (states it makes
    // and the calm replay included): a second harness is refused, not raced.
    let _live = if plan.start.is_production() && !options.force_fixture { Some(Store::lane().lock()?) } else { None };
    let outcome = run(&plan, &out, &options)?;
    print!("{}", outcome.report);
    println!("evidence: {}", out.display());
    Ok(outcome.verdict)
}
