//! Journeys: the end-to-end acceptance (gui-plan §3 item 10). A journey is a
//! person's path through the real desktop (the shell [`super::boot`] mounts,
//! on the fixture index), scripted as acts with content checks at named
//! checkpoints, filmed, and judged.
//!
//! ```text
//! backend-desktop-gui-harness journey NAME|FILE [--out DIR] [--scale 1|2] [--no-fresh] [--index DIR]
//! ```
//!
//! `NAME` is `apps/desktop/journeys/NAME.journey`. The fixture index (the
//! harness's roots) is kept in `--index DIR` (default
//! `.local/harness/journeys/index`), apart from the scenes' copy. The run
//! writes `strip.png` (every checkpoint frame plus the motion in between,
//! from the 2x captures), `motion.json` (the alignment report over the whole
//! journey), `REPORT.txt` (each step and checkpoint with quoted evidence,
//! budgets, settle == fresh), `checks.txt` (every text and target at each
//! checkpoint), `resolved.txt` (the acts as delivered, coordinates resolved)
//! and each checkpoint's full frame. It exits nonzero unless every part
//! passes.
//!
//! A script is one step per line; `#` starts a comment line.
//!
//! | step | meaning |
//! |---|---|
//! | `size WxH`, `start ROUTE` | header: the window and the boot route (`orbit`) |
//! | any `storm --replay` act, untimed | `key cmd-k`, `type "toml Value"`, `resize 480x900`, `text-scale 200`, `motion off`, `hold alt`, `route …`: delivered between two frames |
//! | `click PICK`, `hover PICK` | settle, then the pointer goes to what a person points at: `"WORDS" [after "ANCHOR"] [in AREA]` (the target under those words) or a probe id (`*` globs) |
//! | `STEP else ACT` | `ACT` is a detour, taken only when `STEP` could not be done; the step still fails |
//! | `settle` | frames until nothing is live, nothing is in flight and nothing asks for a frame |
//! | `wait MS` | virtual time passes, frame by frame |
//! | `check NAME` + indented asserts | settle, capture, and judge the settled frame |
//!
//! Asserts, on the settled frame, content first: `route WORDS` (exact; the
//! harness route words), `text "S"… [in AREA]` (each on screen, exactly),
//! `order "A" "B"… [in AREA]` (in this paint order), `absent "S"…`,
//! `line "S" [in AREA]` (a visual row reads S, whitespace aside), `focus
//! PICK`, and `budget page-open|search|flight|frame-p95 <= N ms`. Areas
//! (`titlebar`, `shelf`, `reader`, `pins`, `status`) come from the shell's
//! resolved frame. Every checkpoint also requires zero lints.
//!
//! Budgets. `page-open` and `search` are the latency of the last activation
//! (click, key, type, route): its dispatch, plus the real time spent waiting
//! for reads, plus the virtual time to the first *complete* frame (drawn with
//! every read landed and none issued by its render), plus that frame's draw.
//! A `type` is timed from its last keystroke. `flight` is the virtual time
//! from the last activation to the settle. `frame-p95` covers every frame
//! drawn so far. A debug build measures and reports them but does not judge
//! them.
//!
//! Settle == fresh: the journey ends with the pointer leaving and a settle;
//! a fresh boot then replays the same acts calmly (each one settled before
//! the next) and the two settled frames must be pixel-identical.

mod look;
mod script;

pub use script::{Area, Assert, Budget, Journey, Pick, Step, StepKind};

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
use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The simulated frame period.
const FRAME_MS: u64 = 16;
/// How long a settle may take before the step fails.
const SETTLE_CAP_MS: u64 = 4_000;
/// Film frames after each act (offsets in virtual ms), taken while moving.
const FILM_OFFSETS: [u64; 6] = [0, 96, 224, 416, 640, 960];
/// Strip tile width in physical px.
const TILE_WIDTH: u32 = 960;
/// Budgets are judged only where they mean something.
const JUDGE_BUDGETS: bool = !cfg!(debug_assertions);

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
    line: usize,
    at_ms: u64,
    passed: bool,
    lines: Vec<String>,
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
fn open(start: &str, size: (u32, u32), scale: u8) -> Result<Session, String> {
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

struct Runner {
    session: Session,
    frames: usize,
    last: Option<Seen>,
    moving: bool,
    observed: Vec<Observed>,
    cpu_ms: Vec<f64>,
    events: Vec<Event>,
    undrawn: usize,
    anchor: Option<Anchor>,
    film: Option<Film>,
    tiles: Vec<Tile>,
    filming: bool,
}

impl Runner {
    fn new(session: Session, filming: bool) -> Self {
        Self {
            session,
            frames: 0,
            last: None,
            moving: true,
            observed: Vec::new(),
            cpu_ms: Vec::new(),
            events: Vec::new(),
            undrawn: 0,
            anchor: None,
            film: None,
            tiles: Vec::new(),
            filming,
        }
    }

    fn now(&self) -> u64 {
        self.session.now_ms()
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
        let at = if self.frames == 0 {
            self.now()
        } else {
            (self.now() / FRAME_MS + 1) * FRAME_MS
        };
        self.session.advance_to(at);
        let started = Instant::now();
        self.session.quiesce().map_err(err)?;
        let waited = started.elapsed();
        if let Some(anchor) = self.anchor.as_mut()
            && anchor.open_ms.is_none()
        {
            anchor.wall += waited;
        }
        let film = self.film_due(at);
        let capture = force || film.is_some();
        let (drawn, image) = self.session.frame(capture).map_err(err)?;
        let (ledger, complete, frame, state) = self
            .session
            .update(|_, cx| {
                let ledger = probe::take(cx);
                let before = landed(cx);
                let idle = quiet(cx);
                let state = super::sample_state(cx, &ledger);
                (ledger, idle && landed(cx) == before, shell_frame(cx), state)
            })
            .map_err(err)?;
        self.frames += 1;
        let cpu = drawn.cpu.as_secs_f64() * 1000.0;
        self.cpu_ms.push(cpu);
        if let Some(anchor) = self.anchor.as_mut()
            && anchor.open_ms.is_none()
            && complete
        {
            #[allow(clippy::cast_precision_loss)]
            let virtual_ms = drawn.at_ms.saturating_sub(anchor.at_ms) as f64;
            anchor.open_ms = Some(anchor.wall.as_secs_f64() * 1000.0 + virtual_ms + cpu);
        }
        let mut stripped = ledger.clone();
        stripped.texts.clear();
        stripped.targets.clear();
        stripped.stacks.clear();
        stripped.scrolls.clear();
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
        self.last = Some(Seen {
            drawn,
            ledger,
            complete,
            frame,
        });
        Ok(image)
    }

    /// Frames until two in a row are complete, still, and unrequested.
    /// `Err(words)` says what never settled.
    fn settle(&mut self) -> Result<Result<u64, String>, String> {
        let from = self.now();
        let mut calm_since = None;
        loop {
            self.tick(false)?;
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
            if self.now().saturating_sub(from) > SETTLE_CAP_MS {
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
            self.session
                .apply(act, &mut |act, window, cx| adapt(act, window, cx))
                .map_err(err)?;
            self.events.push(Event {
                at_ms: at,
                act: act.clone(),
            });
            typed |= matches!(act, Act::Type { .. });
        }
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

fn expected_route(words: &str) -> Result<String, String> {
    let target = route::parse(words)?;
    route::to_route(&target, fixture()?).map(|route| describe(&route))
}

/// Every `route` act resolves against the fixture index.
fn routes_resolve(acts: &[Act]) -> Result<(), String> {
    for act in acts {
        if let Act::Route { target } = act {
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
        clippy::cast_precision_loss
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
#[allow(clippy::too_many_lines)]
fn judge_one(runner: &Runner, seen: &Seen, route: Option<&Route>, assert: &Assert) -> Vec<Result<String, String>> {
    match assert {
        Assert::Route(words) => {
            let actual = route.map_or_else(|| "(no route)".to_owned(), describe);
            vec![match expected_route(words) {
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
                    #[allow(clippy::cast_precision_loss)]
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

/// Judges the settled frame against `asserts`, plus zero lints.
fn judge(
    runner: &mut Runner,
    name: &str,
    line: usize,
    asserts: &[Assert],
    image: &RgbaImage,
    unsettled: Option<String>,
) -> Result<Judged, String> {
    let route = runner.session.update(|_, cx| current_route(cx)).map_err(err)?;
    let seen = runner.seen()?;
    let mut lines = Vec::new();
    let mut passed = true;
    if let Some(why) = unsettled {
        passed = false;
        lines.push(format!("  FAIL settle: {why}"));
    }
    for assert in asserts {
        for outcome in judge_one(runner, seen, route.as_ref(), assert) {
            match outcome {
                Ok(evidence) => lines.push(format!("  ok   {evidence}")),
                Err(evidence) => {
                    passed = false;
                    lines.push(format!("  FAIL {evidence}"));
                }
            }
        }
    }
    let linted = lint::lint(image, &seen.ledger, seen.drawn.viewport);
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
        line,
        at_ms: seen.drawn.at_ms,
        passed,
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

/// Replays `events` on a fresh boot, each settled before the next: the
/// frame the journey must equal.
fn calm_replay(journey: &Journey, events: &[Event], scale: u8) -> Result<(RgbaImage, Vec<String>), String> {
    let session = open(&journey.start, journey.size, scale)?;
    let mut runner = Runner::new(session, false);
    let mut notes = Vec::new();
    if let Err(why) = runner.settle()? {
        notes.push(format!("fresh boot: {why}"));
    }
    for event in events {
        runner.deliver(std::slice::from_ref(&event.act), &event.act.to_string())?;
        if let Err(why) = runner.settle()? {
            notes.push(format!("after `{}`: {why}", event.act));
        }
    }
    let image = runner
        .tick(true)?
        .ok_or_else(|| "the calm replay's last frame was not captured".to_owned())?;
    Ok((image, notes))
}

/// Plays one pointer step: settle, locate, deliver (or the detour).
fn point(runner: &mut Runner, step: &Step, click: bool, pick: &Pick, report: &mut String) -> Result<bool, String> {
    if let Err(why) = runner.settle()? {
        let _ = writeln!(report, "  L{:<3} note: not still before `{}`: {why}", step.line, step.text);
    }
    match runner.seen()?.locate(pick) {
        Ok(((x, y), what)) => {
            let act = if click {
                Act::Click {
                    x,
                    y,
                    button: backend_gui_harness::Button::Left,
                }
            } else {
                Act::Move { x, y }
            };
            runner.deliver(std::slice::from_ref(&act), &step.text)?;
            let _ = writeln!(
                report,
                "  L{:<3} {:>6} ms  {}  ->  {act}  ({what})",
                step.line,
                runner.now(),
                step.text
            );
            Ok(true)
        }
        Err(why) => {
            let _ = writeln!(report, "  L{:<3} FAIL {}: {why}", step.line, step.text);
            if let Some(detour) = &step.otherwise {
                match routes_resolve(detour) {
                    Ok(()) => {
                        let words = detour.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
                        runner.deliver(detour, &format!("DETOUR {words}"))?;
                        let _ = writeln!(
                            report,
                            "  L{:<3} {:>6} ms  DETOUR (the person could not do this): {words}",
                            step.line,
                            runner.now()
                        );
                    }
                    Err(why) => {
                        let _ = writeln!(report, "  L{:<3} FAIL detour: {why}", step.line);
                    }
                }
            }
            Ok(false)
        }
    }
}

/// What a finished run printed and whether it passed.
struct Outcome {
    report: String,
    passed: bool,
}

#[allow(clippy::too_many_lines)]
fn run(journey: &Journey, out: &Path, scale: u8, fresh: bool) -> Result<Outcome, String> {
    std::fs::create_dir_all(out).map_err(|error| format!("{}: {error}", out.display()))?;
    let started = Instant::now();
    let session = open(&journey.start, journey.size, scale)?;
    let boot_wall = started.elapsed();
    let mut runner = Runner::new(session, true);
    let mut report = String::new();
    let mut inventories = String::new();
    let mut failed_steps = 0;
    let mut judged: Vec<Judged> = Vec::new();
    let source = short(&journey.source.display().to_string());
    let _ = writeln!(
        report,
        "journey {}  {}x{}@{scale}x abyss 100%  start `{}`\nscript: {source}\nbuild: {}\nboot: {:.1} s wall (fixture index + first reads)",
        journey.name,
        journey.size.0,
        journey.size.1,
        journey.start,
        if JUDGE_BUDGETS { "release" } else { "debug (budgets measured, not judged)" },
        boot_wall.as_secs_f64()
    );
    runner.film = Some(Film {
        label: format!("boot `{}`", journey.start),
        at_ms: 0,
        next: 0,
    });
    if let Err(why) = runner.settle()? {
        failed_steps += 1;
        let _ = writeln!(report, "FAIL boot never settled: {why}");
    }
    let _ = writeln!(report, "\nsteps:");
    for step in &journey.steps {
        match &step.kind {
            StepKind::Acts(acts) => {
                // A route the index cannot resolve is a failed step, not a
                // panic in the product adapter.
                if let Err(why) = routes_resolve(acts) {
                    failed_steps += 1;
                    let _ = writeln!(report, "  L{:<3} FAIL {}: {why}", step.line, step.text);
                    continue;
                }
                runner.deliver(acts, &step.text)?;
                let _ = writeln!(report, "  L{:<3} {:>6} ms  {}", step.line, runner.now(), step.text);
            }
            StepKind::Pointer { click, pick } => {
                if !point(&mut runner, step, *click, pick, &mut report)? {
                    failed_steps += 1;
                }
            }
            StepKind::Settle => match runner.settle()? {
                Ok(_) => {
                    let _ = writeln!(report, "  L{:<3} {:>6} ms  settled", step.line, runner.now());
                }
                Err(why) => {
                    failed_steps += 1;
                    let _ = writeln!(report, "  L{:<3} FAIL settle: {why}", step.line);
                }
            },
            StepKind::Wait(ms) => {
                let until = runner.now() + ms;
                while runner.now() < until {
                    runner.tick(false)?;
                }
                let _ = writeln!(report, "  L{:<3} {:>6} ms  waited {ms} ms", step.line, runner.now());
            }
            StepKind::Check { name, asserts } => {
                let unsettled = runner.settle()?.err();
                let image = runner
                    .tick(true)?
                    .ok_or_else(|| "the checkpoint frame was not captured".to_owned())?;
                let result = judge(&mut runner, name, step.line, asserts, &image, unsettled)?;
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
                let route = runner.session.update(|_, cx| current_route(cx)).map_err(err)?;
                let _ = writeln!(inventories, "check {name} (L{}) t={} ms  frame {file}", step.line, result.at_ms);
                inventories.push_str(&runner.seen()?.inventory(route.as_ref()));
                inventories.push('\n');
                let _ = writeln!(
                    report,
                    "  L{:<3} {:>6} ms  check {name}: {}",
                    step.line,
                    result.at_ms,
                    if result.passed { "PASS" } else { "FAIL" }
                );
                judged.push(result);
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
            "check {} (L{}) at {} ms: {}",
            result.name,
            result.line,
            result.at_ms,
            if result.passed { "PASS" } else { "FAIL" }
        );
        for line in &result.lines {
            let _ = writeln!(report, "{line}");
        }
    }
    // Motion over the whole journey.
    let alignment = align::analyze(&runner.observed, Tolerance::default());
    let _ = writeln!(report, "\nmotion ({} frames, {} ms):", alignment.frames, runner.now());
    for line in align::text(&journey.name, &alignment, 24).lines() {
        let _ = writeln!(report, "  {line}");
    }
    let motion = Json::Obj(vec![
        ("journey".to_owned(), Json::str(journey.name.clone())),
        ("alignment".to_owned(), align::json(&journey.name, &alignment)),
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
    std::fs::write(out.join("motion.json"), motion.to_string()).map_err(err)?;
    let _ = writeln!(
        report,
        "\nframes: {} drawn, draw p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        runner.cpu_ms.len(),
        percentile(&runner.cpu_ms, 0.5),
        percentile(&runner.cpu_ms, 0.95),
        percentile(&runner.cpu_ms, 1.0)
    );
    // Settle == fresh.
    let mut fresh_verdict = "not run";
    if let Some(why) = &end_unsettled {
        fresh_verdict = "FAIL";
        let _ = writeln!(report, "end: FAIL never settled: {why}");
    }
    let replayed = Script {
        events: runner.events.clone(),
    };
    std::fs::write(out.join("resolved.txt"), replayed.to_string()).map_err(err)?;
    if fresh {
        let calm = runner.events.clone();
        match calm_replay(journey, &calm, scale) {
            Ok((image, notes)) => {
                for note in &notes {
                    let _ = writeln!(report, "fresh replay note: {note}");
                }
                match gallery::storm::difference(&end, &image) {
                    None => {
                        if fresh_verdict != "FAIL" {
                            fresh_verdict = "ok";
                        }
                        let _ = writeln!(
                            report,
                            "settle == fresh: identical ({} acts replayed calmly on a fresh boot)",
                            calm.len()
                        );
                    }
                    Some(detail) => {
                        fresh_verdict = "FAIL";
                        save(&image, &out.join("end-fresh.png"))?;
                        let _ = writeln!(
                            report,
                            "settle == fresh: FAIL, {detail} (end-settled.png vs end-fresh.png; {} acts replayed calmly)",
                            calm.len()
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
    let lines = runner.tiles.iter().map(|tile| tile.label.clone()).collect::<Vec<_>>();
    let labels = compose::labels(&lines, TILE_WIDTH / 2, 2, facet::Appearance::Abyss).map_err(err)?;
    let images = runner.tiles.iter().map(|tile| &tile.image).collect::<Vec<_>>();
    let strip = compose::sheet(
        &images,
        &labels,
        4,
        TILE_WIDTH,
        compose::background(facet::Appearance::Abyss),
    );
    save(&strip, &out.join("strip.png"))?;
    std::fs::write(out.join("checks.txt"), inventories).map_err(err)?;
    // The verdict.
    let failed_checks = judged.iter().filter(|result| !result.passed).count();
    let passed = failed_checks == 0 && failed_steps == 0 && alignment.passed() && fresh_verdict == "ok";
    let _ = writeln!(
        report,
        "\nverdict: {}  ({} of {} checkpoints pass; {failed_steps} failed steps; motion {}; settle == fresh {fresh_verdict})\nstrip: {} tiles in strip.png",
        if passed { "PASS" } else { "FAIL" },
        judged.len() - failed_checks,
        judged.len(),
        if alignment.passed() {
            "clean".to_owned()
        } else {
            format!("{} findings", alignment.findings.len())
        },
        runner.tiles.len()
    );
    std::fs::write(out.join("REPORT.txt"), &report).map_err(err)?;
    Ok(Outcome { report, passed })
}

fn usage() -> String {
    "usage: backend-desktop-gui-harness journey NAME|FILE [--out DIR] [--scale 1|2] [--no-fresh] [--index DIR]"
        .to_owned()
}

/// The journey command line.
#[must_use]
pub fn main(args: &[String]) -> ExitCode {
    match command(args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("journey: {error}");
            ExitCode::from(2)
        }
    }
}

fn command(args: &[String]) -> Result<bool, String> {
    let mut name = None;
    let mut out = None;
    let mut scale = 2_u8;
    let mut fresh = true;
    let mut index = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(iter.next().ok_or_else(usage)?)),
            "--index" => index = Some(PathBuf::from(iter.next().ok_or_else(usage)?)),
            "--scale" => {
                scale = iter
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|scale| matches!(scale, 1 | 2))
                    .ok_or_else(usage)?;
            }
            "--no-fresh" => fresh = false,
            other if other.starts_with("--") || name.is_some() => return Err(usage()),
            other => name = Some(other.to_owned()),
        }
    }
    let name = name.ok_or_else(usage)?;
    let path = if Path::new(&name).is_file() {
        PathBuf::from(&name)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("journeys")
            .join(format!("{name}.journey"))
    };
    let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(&name)
        .to_owned();
    let journey = Journey::parse(&stem, &path, &text)?;
    // Journeys keep their own copy of the fixture index (same roots): a
    // journey runs for minutes and would hold the scenes' index lock.
    super::keep_index_in(index.unwrap_or_else(|| super::repo().join(".local/harness/journeys/index")));
    let out = out.unwrap_or_else(|| super::repo().join(".local/harness/journeys").join(&stem));
    let outcome = run(&journey, &out, scale, fresh)?;
    print!("{}", outcome.report);
    println!("evidence: {}", out.display());
    Ok(outcome.passed)
}
