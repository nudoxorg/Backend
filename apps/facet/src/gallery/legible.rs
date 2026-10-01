//! The legibility law on a film (DIRECTION §1 law 2, §3 enforcement): at
//! every sampled frame of a motion, each probed text is either legible and
//! unoverlapped, or not drawn at all.
//!
//! A film is sampled every [`STEP_MS`] of virtual time (each sample a genuine
//! draw). Per frame, for every text line gpui painted (the window's
//! [`gpui::TextTrace`], so nothing needs wrapping in a probe to be seen):
//!
//! | Rule | Fails when |
//! |---|---|
//! | `overlap` | two texts whose visible boxes intersect by more than 1 px in both axes are both readable (ink contrast at least [`READABLE`] in the part of each box the other does not cover) and both seen at the crossing: the crossing's ground is each text's own ground, so neither sits under the other's opaque plate |
//! | `faded` | a text is painted translucent (alpha under 0.95) and drawn (contrast above [`SEEN`]) but under [`FADED`] of its own best contrast in the film, for more than [`FADED_FRAMES`] consecutive frames: a fade or cross-fade in progress |
//!
//! Contrast is measured from the frame's pixels exactly as [`super::lint`]
//! measures it (the box's most common colour is the ground, the pixel that
//! contrasts with it most the ink), so a text under an opaque plate, clipped
//! away or at zero opacity is simply not drawn. Pixels only resolve glyph
//! stems at 2x, so the check refuses other scales.

use super::json::Json;
use super::lint::ink_contrast;
use crate::probe::BoundsSample;
use gpui::PaintedText;
use image::RgbaImage;
use std::collections::{BTreeMap, HashMap};

/// Sampling period of a legibility film, ms (one 60 Hz frame).
pub const STEP_MS: u64 = 16;
/// Contrast at which a text is readable (and so may not be crossed).
pub const READABLE: f32 = 1.8;
/// Contrast at or below which a text is not drawn at all.
pub const SEEN: f32 = 1.15;
/// Share of its own best contrast under which a drawn text is fading.
pub const FADED: f32 = 0.8;
/// Consecutive faded frames tolerated (a text may pass through a fade this
/// fast, never linger in one).
pub const FADED_FRAMES: usize = 2;

/// A rule of the law.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub enum Rule {
    /// Two readable texts intersect.
    Overlap,
    /// A drawn text under legibility for too long.
    Faded,
}

impl Rule {
    /// A stable name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Overlap => "overlap",
            Self::Faded => "faded",
        }
    }
}

/// One violation, over a run of consecutive frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// The rule.
    pub rule: Rule,
    /// The text (`a × b` for an overlap), by content.
    pub what: String,
    /// The probe keys involved.
    pub keys: Vec<String>,
    /// First and last frame (virtual ms) of the run.
    pub from_ms: u64,
    /// Last frame of the run.
    pub to_ms: u64,
    /// Frames in the run.
    pub frames: usize,
    /// The worst contrast(s) seen, for the record.
    pub detail: String,
    /// Whether the pair also overlaps in a frame where nothing animates (a
    /// layout overlap at rest, not a motion).
    pub at_rest: bool,
}

/// What one frame measured for one text.
#[derive(Clone, Debug)]
struct Seen {
    key: String,
    content: String,
    visible: BoundsSample,
    /// Contrast in the part of the box no other text covers (the whole box
    /// when it is covered entirely).
    contrast: f32,
    /// The alpha its ink was painted at.
    alpha: f32,
    /// The ground it is read on (where no other text covers it).
    ground: Option<[u8; 3]>,
}

/// One sampled frame, measured (the image is dropped after measuring).
#[derive(Clone, Debug, Default)]
pub struct Measured {
    /// Virtual time, ms.
    pub at_ms: u64,
    texts: Vec<Seen>,
    /// Text lines painted in the frame.
    pub probed: usize,
    /// Whether nothing was animating (no live motion track): a rest frame.
    pub idle: bool,
    /// Pairs of texts whose boxes intersect and that are both seen there
    /// (sorted keys).
    crossings: Vec<(String, String)>,
}

/// Whether two grounds are the same paint (within antialiasing noise).
fn same_ground(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 8)
}

/// The film's verdict.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Frames sampled.
    pub frames: usize,
    /// Distinct texts seen drawn in any frame.
    pub texts: usize,
    /// Most probed texts in one frame.
    pub probed: usize,
    /// Violations, in time order.
    pub findings: Vec<Finding>,
}

impl Report {
    /// Whether every frame held the law.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.findings.is_empty()
    }
}

fn intersect(a: &BoundsSample, b: &BoundsSample) -> Option<BoundsSample> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    (right - x > 1.0 && bottom - y > 1.0).then(|| BoundsSample {
        key: String::new(),
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

/// The largest strip of `a` outside `cut` (left, right, above or below it).
fn outside(a: &BoundsSample, cut: &BoundsSample) -> Option<BoundsSample> {
    let strips = [
        (a.x, a.y, cut.x - a.x, a.height),
        (
            cut.x + cut.width,
            a.y,
            a.x + a.width - (cut.x + cut.width),
            a.height,
        ),
        (a.x, a.y, a.width, cut.y - a.y),
        (
            a.x,
            cut.y + cut.height,
            a.width,
            a.y + a.height - (cut.y + cut.height),
        ),
    ];
    strips
        .into_iter()
        .filter(|(_, _, w, h)| *w > 1.0 && *h > 1.0)
        .max_by(|p, q| (p.2 * p.3).total_cmp(&(q.2 * q.3)))
        .map(|(x, y, width, height)| BoundsSample {
            key: a.key.clone(),
            x,
            y,
            width,
            height,
        })
}

/// Measures one frame: every painted text line's visible box and its
/// contrast where no other text covers it. Lines are keyed by their text and
/// its occurrence in paint order (`name#2` is the second `name` painted).
#[must_use]
pub fn measure(
    image: &RgbaImage,
    painted: &[PaintedText],
    scale: u8,
    at_ms: u64,
    idle: bool,
) -> Measured {
    #[allow(clippy::cast_precision_loss)]
    let (width, height) = (
        image.width() as f32 / f32::from(scale),
        image.height() as f32 / f32::from(scale),
    );
    let mut seen_count: HashMap<&str, usize> = HashMap::new();
    let boxes: Vec<(String, String, BoundsSample, f32)> = painted
        .iter()
        .filter_map(|line| {
            let count = seen_count.entry(line.text.as_ref()).or_default();
            *count += 1;
            let key = if *count == 1 {
                line.text.to_string()
            } else {
                format!("{}#{count}", line.text)
            };
            let (x, y) = (
                f32::from(line.bounds.origin.x).max(0.0),
                f32::from(line.bounds.origin.y).max(0.0),
            );
            let right = f32::from(line.bounds.origin.x + line.bounds.size.width).min(width);
            let bottom = f32::from(line.bounds.origin.y + line.bounds.size.height).min(height);
            (right - x > 1.0 && bottom - y > 1.0 && line.alpha > 0.0).then(|| {
                let visible = BoundsSample {
                    key: key.clone(),
                    x,
                    y,
                    width: right - x,
                    height: bottom - y,
                };
                (key, line.text.to_string(), visible, line.alpha)
            })
        })
        .collect();
    let contrast = |bounds: &BoundsSample| {
        ink_contrast(
            image,
            scale,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
        )
        .map_or((1.0, None), |(ratio, _, ground)| (ratio, Some(ground)))
    };
    let texts: Vec<Seen> = boxes
        .iter()
        .map(|(key, content, visible, alpha)| {
            // The largest part of it another text covers.
            let covered = boxes
                .iter()
                .filter(|(other, ..)| other != key)
                .filter_map(|(_, _, other, _)| intersect(visible, other))
                .max_by(|p, q| (p.width * p.height).total_cmp(&(q.width * q.height)));
            let region = covered
                .and_then(|cut| outside(visible, &cut))
                .unwrap_or_else(|| visible.clone());
            let (contrast, ground) = contrast(&region);
            Seen {
                key: key.clone(),
                content: content.clone(),
                visible: visible.clone(),
                contrast,
                alpha: *alpha,
                ground,
            }
        })
        .collect();
    // Where two boxes intersect, both texts are seen only if the crossing is
    // read on both texts' own ground: an opaque plate under one of them (a
    // card over the page) hides the other there.
    let mut crossings = Vec::new();
    for (i, a) in texts.iter().enumerate() {
        for b in &texts[i + 1..] {
            let Some(cut) = intersect(&a.visible, &b.visible) else {
                continue;
            };
            let (Some(own_a), Some(own_b)) = (a.ground, b.ground) else {
                continue;
            };
            let seen = ink_contrast(image, scale, cut.x, cut.y, cut.width, cut.height).is_some_and(
                |(_, _, ground)| same_ground(ground, own_a) && same_ground(ground, own_b),
            );
            if seen {
                let (first, second) = if a.key <= b.key { (a, b) } else { (b, a) };
                crossings.push((first.key.clone(), second.key.clone()));
            }
        }
    }
    Measured {
        at_ms,
        texts,
        probed: painted.len(),
        idle,
        crossings,
    }
}

fn short(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 28 {
        format!("{}…", flat.chars().take(27).collect::<String>())
    } else {
        flat
    }
}

/// Judges a measured film.
#[must_use]
pub fn judge(film: &[Measured]) -> Report {
    // Each text's best contrast anywhere in the film.
    let mut best: HashMap<&str, f32> = HashMap::new();
    for frame in film {
        for text in &frame.texts {
            let entry = best.entry(text.key.as_str()).or_insert(0.0);
            *entry = entry.max(text.contrast);
        }
    }
    let rest_in = |frame: Option<&Measured>, a: &str, b: &str| {
        frame.is_some_and(|frame| {
            let find = |key: &str| frame.texts.iter().find(|text| text.key == key);
            match (find(a), find(b)) {
                (Some(a), Some(b)) => {
                    a.contrast >= READABLE
                        && b.contrast >= READABLE
                        && frame
                            .crossings
                            .iter()
                            .any(|(x, y)| x == &a.key && y == &b.key)
                }
                _ => false,
            }
        })
    };
    // Open runs: (rule, keys) -> (finding, last frame index).
    let mut open: BTreeMap<(Rule, Vec<String>), (Finding, usize)> = BTreeMap::new();
    let mut done = Vec::new();
    for (index, frame) in film.iter().enumerate() {
        let mut hits: Vec<(Rule, Vec<String>, String, String)> = Vec::new();
        for (i, a) in frame.texts.iter().enumerate() {
            for b in &frame.texts[i + 1..] {
                if a.contrast < READABLE || b.contrast < READABLE {
                    continue;
                }
                let (a, b) = if a.key <= b.key { (a, b) } else { (b, a) };
                if !frame
                    .crossings
                    .iter()
                    .any(|(x, y)| *x == a.key && *y == b.key)
                {
                    continue;
                }
                hits.push((
                    Rule::Overlap,
                    vec![a.key.clone(), b.key.clone()],
                    format!("`{}` × `{}`", short(&a.content), short(&b.content)),
                    format!("{:.2}:1 and {:.2}:1", a.contrast, b.contrast),
                ));
            }
            let own = best.get(a.key.as_str()).copied().unwrap_or(0.0);
            if own >= READABLE && a.alpha < 0.95 && a.contrast > SEEN && a.contrast < FADED * own {
                hits.push((
                    Rule::Faded,
                    vec![a.key.clone()],
                    format!("`{}`", short(&a.content)),
                    format!("{:.2}:1 of its {:.2}:1", a.contrast, own),
                ));
            }
        }
        for (rule, keys, what, detail) in hits {
            let run = open.entry((rule, keys.clone())).or_insert_with(|| {
                (
                    Finding {
                        rule,
                        what,
                        at_rest: rule == Rule::Overlap
                            && film
                                .iter()
                                .filter(|frame| frame.idle)
                                .any(|frame| rest_in(Some(frame), &keys[0], &keys[1])),
                        keys,
                        from_ms: frame.at_ms,
                        to_ms: frame.at_ms,
                        frames: 0,
                        detail: String::new(),
                    },
                    index,
                )
            });
            run.0.to_ms = frame.at_ms;
            run.0.frames += 1;
            run.0.detail = detail;
            run.1 = index;
        }
        // Close runs that did not continue into this frame.
        let closed: Vec<_> = open
            .iter()
            .filter(|(_, (_, last))| *last != index)
            .map(|(key, _)| key.clone())
            .collect();
        for key in closed {
            if let Some((finding, _)) = open.remove(&key) {
                done.push(finding);
            }
        }
    }
    done.extend(open.into_values().map(|(finding, _)| finding));
    let findings = done
        .into_iter()
        .filter(|finding| finding.rule == Rule::Overlap || finding.frames > FADED_FRAMES)
        .collect::<Vec<_>>();
    let mut findings = findings;
    findings.sort_by(|a, b| (a.from_ms, a.rule, &a.keys).cmp(&(b.from_ms, b.rule, &b.keys)));
    Report {
        frames: film.len(),
        texts: best.values().filter(|&&contrast| contrast > SEEN).count(),
        probed: film.iter().map(|frame| frame.probed).max().unwrap_or(0),
        findings,
    }
}

/// The report as text: a verdict line, coverage, then one line per finding.
#[must_use]
pub fn text(scene: &str, report: &Report) -> String {
    let mut out = format!(
        "legibility  {scene}  {} frames @{STEP_MS} ms  {} texts drawn (max {} probed in a frame)  {}\n",
        report.frames,
        report.texts,
        report.probed,
        if report.passed() { "PASS" } else { "FAIL" }
    );
    for finding in &report.findings {
        out.push_str(&format!(
            "  FAIL {:<8} {:>5}..{:<5} ms ({:>2} frames)  {}  [{}]{}\n",
            finding.rule.name(),
            finding.from_ms,
            finding.to_ms,
            finding.frames,
            finding.what,
            finding.detail,
            if finding.at_rest {
                "  (also at rest)"
            } else {
                ""
            }
        ));
    }
    out
}

/// One measured frame as JSON: every drawn text with its box, contrast and
/// alpha (for storyboards and ledgers that need the geometry).
#[must_use]
pub fn frame_json(frame: &Measured) -> Json {
    Json::obj([
        ("at_ms", Json::num(frame.at_ms as f64)),
        ("idle", Json::Bool(frame.idle)),
        (
            "texts",
            Json::Arr(
                frame
                    .texts
                    .iter()
                    .map(|text| {
                        Json::obj([
                            ("key", Json::str(text.key.clone())),
                            ("content", Json::str(text.content.clone())),
                            ("x", Json::num(f64::from(text.visible.x))),
                            ("y", Json::num(f64::from(text.visible.y))),
                            ("w", Json::num(f64::from(text.visible.width))),
                            ("h", Json::num(f64::from(text.visible.height))),
                            ("contrast", Json::num(f64::from(text.contrast))),
                            ("alpha", Json::num(f64::from(text.alpha))),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

/// The report as JSON.
#[must_use]
pub fn json(scene: &str, report: &Report) -> Json {
    Json::obj([
        ("scene", Json::str(scene)),
        ("pass", Json::Bool(report.passed())),
        ("frames", Json::num(report.frames as f64)),
        ("step_ms", Json::num(STEP_MS as f64)),
        ("texts", Json::num(report.texts as f64)),
        ("probed", Json::num(report.probed as f64)),
        (
            "findings",
            Json::Arr(
                report
                    .findings
                    .iter()
                    .map(|finding| {
                        Json::obj([
                            ("rule", Json::str(finding.rule.name())),
                            ("what", Json::str(finding.what.clone())),
                            (
                                "keys",
                                Json::Arr(
                                    finding
                                        .keys
                                        .iter()
                                        .map(|key| Json::str(key.clone()))
                                        .collect(),
                                ),
                            ),
                            ("from_ms", Json::num(finding.from_ms as f64)),
                            ("to_ms", Json::num(finding.to_ms as f64)),
                            ("frames", Json::num(finding.frames as f64)),
                            ("detail", Json::str(finding.detail.clone())),
                            ("at_rest", Json::Bool(finding.at_rest)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

/// One transition of a scripted film: the window from an act to the next
/// act at a later instant (acts at one instant share a window).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// What the catalog calls it: the act line's trailing `# comment`, or
    /// the act itself.
    pub label: String,
    /// The act(s) that open it, as scripted (time stripped).
    pub act: String,
    /// When the act lands, virtual ms.
    pub from_ms: u64,
    /// When the next window opens (exclusive); `None` runs to the film's end.
    pub to_ms: Option<u64>,
}

/// The transition windows of a film script: one per scripted instant, named
/// by the act line's trailing `# comment` (a comment-only line names
/// nothing). Times follow the script syntax: `@T` absolute, `+T` after the
/// previous statement, none at the previous statement's instant.
#[must_use]
pub fn windows(source: &str) -> Vec<Window> {
    let mut out: Vec<Window> = Vec::new();
    let mut previous = 0_u64;
    for line in source.lines() {
        let (code, label) = split_comment(line);
        for statement in code
            .split(';')
            .map(str::trim)
            .filter(|statement| !statement.is_empty())
        {
            let mut words: Vec<&str> = statement.split_whitespace().collect();
            let mut at = previous;
            if let Some(last) = words.last().copied() {
                if let Some(time) = last
                    .strip_prefix('@')
                    .and_then(|time| time.parse::<u64>().ok())
                {
                    at = time;
                    words.pop();
                } else if let Some(delta) = last
                    .strip_prefix('+')
                    .and_then(|delta| delta.parse::<u64>().ok())
                {
                    at = previous.saturating_add(delta);
                    words.pop();
                }
            }
            previous = at;
            let act = words.join(" ");
            let label = label.clone().unwrap_or_else(|| act.clone());
            match out.last_mut() {
                Some(window) if window.from_ms == at => {
                    window.act = format!("{}; {act}", window.act);
                    if window.label != label {
                        window.label = format!("{} + {label}", window.label);
                    }
                }
                _ => {
                    if let Some(window) = out.last_mut() {
                        window.to_ms = Some(at);
                    }
                    out.push(Window {
                        label,
                        act,
                        from_ms: at,
                        to_ms: None,
                    });
                }
            }
        }
    }
    out
}

/// A line's statements and its trailing `# comment` (outside quotes).
fn split_comment(line: &str) -> (&str, Option<String>) {
    let mut quoted = false;
    for (at, ch) in line.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '#' if !quoted => {
                let comment = line[at + 1..].trim();
                return (
                    &line[..at],
                    (!comment.is_empty()).then(|| comment.to_owned()),
                );
            }
            _ => {}
        }
    }
    (line, None)
}

/// What one transition window did against the law.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Text changed in the window and every frame held the law.
    Pass,
    /// Some frame broke the law.
    Fail,
    /// Every frame held the law, but no drawn text changed: the act did
    /// nothing visible, so the pass proves nothing.
    NotExercised,
}

impl Verdict {
    /// A stable name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::NotExercised => "NOT EXERCISED",
        }
    }
}

/// One line of the ledger: a window and what the law found in it.
#[derive(Clone, Debug)]
pub struct Transition {
    /// The window.
    pub window: Window,
    /// Motion overlaps (pairs that never overlap at rest), by first frame.
    pub overlap: Vec<Finding>,
    /// Faded runs.
    pub faded: Vec<Finding>,
    /// Overlaps that also exist at rest (layout, not motion).
    pub at_rest: Vec<Finding>,
    /// Texts drawn in the window that were not drawn as it opened.
    pub new: usize,
    /// Texts drawn as it opened that are gone by its end.
    pub gone: usize,
}

impl Transition {
    /// The window's verdict.
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        if !self.overlap.is_empty() || !self.faded.is_empty() {
            Verdict::Fail
        } else if self.new + self.gone == 0 {
            Verdict::NotExercised
        } else {
            Verdict::Pass
        }
    }

    /// The longest violating run, ms.
    #[must_use]
    pub fn longest_ms(&self) -> u64 {
        self.overlap
            .iter()
            .chain(&self.faded)
            .map(|finding| finding.to_ms - finding.from_ms + STEP_MS)
            .max()
            .unwrap_or(0)
    }

    /// How long after the act the last violating frame is, ms.
    #[must_use]
    pub fn last_ms(&self) -> u64 {
        self.overlap
            .iter()
            .chain(&self.faded)
            .map(|finding| finding.to_ms.saturating_sub(self.window.from_ms))
            .max()
            .unwrap_or(0)
    }
}

/// Assigns a film's findings to its transition windows (each finding to the
/// window its first frame falls in) and measures whether each window
/// changed any drawn text.
#[must_use]
pub fn ledger(film: &[Measured], report: &Report, windows: &[Window]) -> Vec<Transition> {
    let drawn = |frame: &Measured| {
        frame
            .texts
            .iter()
            .filter(|text| text.contrast >= READABLE)
            .map(|text| text.key.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    windows
        .iter()
        .map(|window| {
            let inside = |at: u64| at >= window.from_ms && window.to_ms.is_none_or(|to| at < to);
            let mut overlap = Vec::new();
            let mut faded = Vec::new();
            let mut at_rest = Vec::new();
            for finding in report
                .findings
                .iter()
                .filter(|finding| inside(finding.from_ms))
            {
                match (finding.rule, finding.at_rest) {
                    (Rule::Overlap, false) => overlap.push(finding.clone()),
                    (Rule::Overlap, true) => at_rest.push(finding.clone()),
                    (Rule::Faded, _) => faded.push(finding.clone()),
                }
            }
            let start = film
                .iter()
                .rev()
                .find(|frame| frame.at_ms < window.from_ms)
                .or_else(|| film.iter().find(|frame| inside(frame.at_ms)))
                .map(drawn)
                .unwrap_or_default();
            let frames: Vec<&Measured> = film.iter().filter(|frame| inside(frame.at_ms)).collect();
            let seen: std::collections::BTreeSet<String> =
                frames.iter().flat_map(|frame| drawn(frame)).collect();
            let end = frames.last().map(|frame| drawn(frame)).unwrap_or_default();
            Transition {
                window: window.clone(),
                overlap,
                faded,
                at_rest,
                new: seen.difference(&start).count(),
                gone: start.difference(&end).count(),
            }
        })
        .collect()
}

/// The ledger as text: one verdict line per transition, then up to three
/// overlaps and two faded runs as examples.
#[must_use]
pub fn ledger_text(film: &str, transitions: &[Transition]) -> String {
    let mut out = String::new();
    for transition in transitions {
        let window = &transition.window;
        out.push_str(&format!(
            "{:<13} {film}  {}  (`{}` @{})  overlap {}  faded {}  (+{} at rest)  longest {} ms  last +{} ms  texts +{} -{}\n",
            transition.verdict().name(),
            window.label,
            window.act,
            window.from_ms,
            transition.overlap.len(),
            transition.faded.len(),
            transition.at_rest.len(),
            transition.longest_ms(),
            transition.last_ms(),
            transition.new,
            transition.gone,
        ));
        let mut overlap = transition.overlap.iter().collect::<Vec<_>>();
        overlap.sort_by_key(|finding| std::cmp::Reverse(finding.frames));
        let mut faded = transition.faded.iter().collect::<Vec<_>>();
        faded.sort_by_key(|finding| std::cmp::Reverse(finding.frames));
        for finding in overlap.into_iter().take(3).chain(faded.into_iter().take(2)) {
            out.push_str(&format!(
                "      {:<7} {}..{} ms: {} [{}]\n",
                finding.rule.name(),
                finding.from_ms,
                finding.to_ms,
                finding.what,
                finding.detail
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Measured, Rule, Seen, judge};
    use crate::probe::BoundsSample;

    fn seen(key: &str, x: f32, y: f32, contrast: f32) -> Seen {
        Seen {
            key: key.to_owned(),
            content: key.to_owned(),
            visible: BoundsSample {
                key: key.to_owned(),
                x,
                y,
                width: 100.0,
                height: 20.0,
            },
            contrast,
            alpha: (contrast - 1.0) / 7.0,
            ground: Some([10, 12, 20]),
        }
    }

    /// A frame where every intersecting pair is seen on one ground.
    fn frame(at_ms: u64, texts: Vec<Seen>) -> Measured {
        let mut crossings = Vec::new();
        for (i, a) in texts.iter().enumerate() {
            for b in &texts[i + 1..] {
                if super::intersect(&a.visible, &b.visible).is_some() {
                    let (x, y) = if a.key <= b.key { (a, b) } else { (b, a) };
                    crossings.push((x.key.clone(), y.key.clone()));
                }
            }
        }
        Measured {
            at_ms,
            probed: texts.len(),
            texts,
            idle: false,
            crossings,
        }
    }

    #[test]
    fn two_readable_texts_crossing_fail_and_a_text_under_its_ground_does_not() {
        // A title flies down through a lede at 50 px; the lede is readable.
        let film: Vec<_> = (0..6)
            .map(|k| {
                let y = k as f32 * 20.0;
                frame(
                    k * 16,
                    vec![seen("title", 0.0, y, 8.0), seen("lede", 0.0, 50.0, 6.0)],
                )
            })
            .collect();
        let report = judge(&film);
        let overlaps: Vec<_> = report
            .findings
            .iter()
            .filter(|f| f.rule == Rule::Overlap)
            .collect();
        assert_eq!(overlaps.len(), 1, "{report:?}");
        assert_eq!(
            (overlaps[0].from_ms, overlaps[0].to_ms),
            (32, 48),
            "{report:?}"
        );
        // The same flight over a lede that is not drawn (under its ground).
        let film: Vec<_> = (0..6)
            .map(|k| {
                let y = k as f32 * 20.0;
                frame(
                    k * 16,
                    vec![seen("title", 0.0, y, 8.0), seen("lede", 0.0, 50.0, 1.05)],
                )
            })
            .collect();
        assert!(judge(&film).passed(), "{:?}", judge(&film));
    }

    /// Pixels: a text on the page and a text on a plate that overlaps it.
    /// On one shared ground the crossing is seen and fails; when the second
    /// text sits on an opaque card that covers the first there, the first is
    /// hidden at the crossing and nothing fails.
    #[test]
    fn a_card_covering_page_text_hides_it_and_text_on_one_ground_crosses() {
        use gpui::{PaintedText, SharedString, point, px, size};
        use image::{Rgba, RgbaImage};
        let ground = Rgba([10, 12, 20, 255]);
        let card = Rgba([40, 48, 70, 255]);
        let ink = Rgba([235, 238, 245, 255]);
        let paint = |with_card: bool| {
            // 120 x 60 logical at 2x.
            let mut image = RgbaImage::from_pixel(240, 120, ground);
            let mut glyphs = |image: &mut RgbaImage, x0: u32, y0: u32, w: u32| {
                // Thin stems: ink never outnumbers the ground it is read on.
                for x in (x0..x0 + w).step_by(5) {
                    for y in y0 + 4..y0 + 16 {
                        image.put_pixel(x, y, ink);
                    }
                }
            };
            glyphs(&mut image, 20, 20, 100); // page text: logical (10,10) 50 x 10
            if with_card {
                for x in 70..200 {
                    for y in 10..70 {
                        image.put_pixel(x, y, card);
                    }
                }
            }
            glyphs(&mut image, 80, 24, 100); // card text: logical (40,12) 50 x 10
            image
        };
        let line = |text: &str, x: f32, y: f32| PaintedText {
            text: SharedString::from(text.to_owned()),
            bounds: gpui::Bounds::new(point(px(x), px(y)), size(px(50.0), px(10.0))),
            alpha: 1.0,
        };
        let lines = [line("page", 10.0, 10.0), line("card", 40.0, 12.0)];
        let film = |with_card: bool| vec![super::measure(&paint(with_card), &lines, 2, 0, true)];
        let crossed = judge(&film(false));
        assert!(
            crossed.findings.iter().any(|f| f.rule == Rule::Overlap),
            "two texts on one ground: {crossed:?}"
        );
        let covered = judge(&film(true));
        assert!(
            covered.passed(),
            "a card hides the page text it covers: {covered:?}"
        );
    }

    #[test]
    fn a_film_script_splits_into_named_windows_and_the_ledger_places_each_finding_once() {
        use super::{Verdict, Window, ledger, windows};
        let source = "# the film\nroute package present @320  # route-down\nkey cmd-[ @640 # back\nmove 1,1\nkey j +160\n";
        let named = windows(source);
        assert_eq!(
            named,
            vec![
                Window {
                    label: "route-down".into(),
                    act: "route package present".into(),
                    from_ms: 320,
                    to_ms: Some(640)
                },
                Window {
                    label: "back + move 1,1".into(),
                    act: "key cmd-[; move 1,1".into(),
                    from_ms: 640,
                    to_ms: Some(800)
                },
                Window {
                    label: "key j".into(),
                    act: "key j".into(),
                    from_ms: 800,
                    to_ms: None
                },
            ]
        );
        // `old` is drawn until 320; `new` arrives at 336 and crosses `old`
        // for two frames; after 640 nothing changes; at 800 `j` appears.
        let film: Vec<_> = (0..60_u64)
            .map(|k| {
                let at = k * 16;
                let mut texts = Vec::new();
                if at < 368 {
                    texts.push(seen("old", 0.0, 0.0, 8.0));
                }
                if at >= 336 {
                    texts.push(seen("new", 0.0, if at < 368 { 10.0 } else { 40.0 }, 8.0));
                }
                if at >= 800 {
                    texts.push(seen("j", 200.0, 0.0, 8.0));
                }
                frame(at, texts)
            })
            .collect();
        let report = judge(&film);
        let rows = ledger(&film, &report, &named);
        assert_eq!(rows[0].verdict(), Verdict::Fail, "{rows:?}");
        assert_eq!(
            (rows[0].overlap.len(), rows[0].new, rows[0].gone),
            (1, 1, 1),
            "{rows:?}"
        );
        assert_eq!(
            (rows[0].overlap[0].from_ms, rows[0].last_ms()),
            (336, 32),
            "{rows:?}"
        );
        assert_eq!(rows[1].verdict(), Verdict::NotExercised, "{rows:?}");
        assert_eq!(rows[2].verdict(), Verdict::Pass, "{rows:?}");
    }

    #[test]
    fn a_fade_that_lingers_fails_and_a_cut_does_not() {
        // A 160 ms fade in: ten frames between not drawn and its best.
        let film: Vec<_> = (0..12)
            .map(|k| {
                frame(
                    k * 16,
                    vec![seen(
                        "page",
                        0.0,
                        0.0,
                        1.0 + 7.0 * (k as f32 / 10.0).min(1.0),
                    )],
                )
            })
            .collect();
        let report = judge(&film);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.rule == Rule::Faded && f.frames > 2),
            "{report:?}"
        );
        // A cut: not drawn, then at its best.
        let film: Vec<_> = (0..12)
            .map(|k| {
                frame(
                    k * 16,
                    vec![seen("page", 0.0, 0.0, if k < 6 { 1.0 } else { 8.0 })],
                )
            })
            .collect();
        assert!(judge(&film).passed());
    }
}
