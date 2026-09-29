//! The journey line grammar: acts (the `storm --replay` language, untimed),
//! picks of what a person points at, the steps a production journey adds
//! (`await`, `answer-picker`, `restart`), and checkpoints of content asserts.
//! Assembling lines into a plan (headers, `do PART`, `needs`) is
//! [`super::plan`]'s.

use super::super::route;
use backend_gui_harness::{Act, Script};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Where a line was written: a file and its 1-based line (a Rust call site
/// for a plan built in code).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Site {
    /// The file.
    pub file: Arc<Path>,
    /// The 1-based line.
    pub line: usize,
}

impl Site {
    /// `file:line`.
    #[must_use]
    pub fn new(file: &Path, line: usize) -> Self {
        Self { file: Arc::from(file), line }
    }
}

impl std::fmt::Display for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = super::look::short(&self.file.display().to_string()).replace("<repo>/apps/desktop/journeys/", "");
        write!(f, "{shown}:{}", self.line)
    }
}

/// The sites a step came through: the journey's own line first (a `do`
/// line when the step comes from a part), the line that wrote it last.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Origin(pub Vec<Site>);

impl Origin {
    /// One site.
    #[must_use]
    pub fn at(site: Site) -> Self {
        Self(vec![site])
    }

    /// This origin, then `site` (a part's line, used from here).
    #[must_use]
    pub fn then(&self, site: Site) -> Self {
        let mut sites = self.0.clone();
        sites.push(site);
        Self(sites)
    }

    /// The line that wrote the step.
    #[must_use]
    pub fn last(&self) -> Option<&Site> {
        self.0.last()
    }
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, site) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(" > ")?;
            }
            write!(f, "{site}")?;
        }
        Ok(())
    }
}

/// One step, with where it came from.
#[derive(Clone, Debug)]
pub struct Step {
    /// The sites it came through (journey line first).
    pub origin: Origin,
    /// The line as delivered (parameters substituted).
    pub text: String,
    /// What it does.
    pub kind: StepKind,
    /// The detour taken when the step cannot be done: acts or a pointer.
    /// Taking it still fails the step.
    pub otherwise: Option<Box<StepKind>>,
    /// A product or data gap this step waits on: when it fails, the journey
    /// is BLOCKED with these words, not FAIL.
    pub needs: Option<Gap>,
}

/// What kind of gap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GapKind {
    /// A feature the product does not have yet.
    Product,
    /// Data the index cannot serve yet.
    Data,
}

impl GapKind {
    /// The report spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Product => "product",
            Self::Data => "data",
        }
    }
}

/// A named gap (`needs product "…"`): the verdict's words when the step it
/// gates fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Gap {
    /// Product or data.
    pub kind: GapKind,
    /// What is missing, in the words GAPS.md uses.
    pub what: String,
}

/// What an `await` waits for.
#[derive(Clone, Debug, PartialEq)]
pub enum Until {
    /// Every string is on screen (in the area).
    Text(Vec<String>, Option<Area>),
    /// No string is on screen (in the area).
    Absent(Vec<String>, Option<Area>),
    /// Some text on screen (in the area) matches the glob (`"All * packages
    /// toml_pin uses are in the library."`).
    Like(String, Option<Area>),
}

/// How the harness answers the native folder panel the product opened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickerAnswer {
    /// The person chose these folders (absolute).
    Folders(Vec<PathBuf>),
    /// The person cancelled.
    Cancel,
}

/// What a step does.
#[derive(Clone, Debug)]
pub enum StepKind {
    /// Acts delivered at one instant.
    Acts(Vec<Act>),
    /// The pointer goes to (and clicks) what a person points at.
    Pointer {
        /// Click, or only hover.
        click: bool,
        /// What.
        pick: Pick,
    },
    /// Frames until still.
    Settle,
    /// Virtual time passes.
    Wait(u64),
    /// Real time passes, frames drawn, until the owner's work shows the
    /// condition (indexing runs for minutes on real threads).
    Await {
        /// The condition.
        until: Until,
        /// The real-time bound.
        within: Duration,
    },
    /// The native folder panel the product opened is answered.
    AnswerPicker(PickerAnswer),
    /// Quit (the app's quit handlers run, the owner stops) and launch again
    /// on the same machine state, through the production launch path.
    Restart,
    /// A named checkpoint.
    Check {
        /// Its name.
        name: String,
        /// What must be true.
        asserts: Vec<Assert>,
    },
}

/// A part of the window, from the shell's resolved frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Area {
    /// The titlebar.
    Titlebar,
    /// The shelf column (or spine).
    Shelf,
    /// The page.
    Reader,
    /// The pinned peeks column.
    Pins,
    /// The status bar.
    Status,
}

impl Area {
    /// The script spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Titlebar => "titlebar",
            Self::Shelf => "shelf",
            Self::Reader => "reader",
            Self::Pins => "pins",
            Self::Status => "status",
        }
    }

    fn parse(word: &str) -> Result<Self, String> {
        Ok(match word {
            "titlebar" => Self::Titlebar,
            "shelf" => Self::Shelf,
            "reader" => Self::Reader,
            "pins" => Self::Pins,
            "status" => Self::Status,
            other => {
                return Err(format!(
                    "`{other}` is not an area (titlebar, shelf, reader, pins, status)"
                ));
            }
        })
    }
}

/// What a person points at (or what should hold the focus).
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// The one target published under this key (`*` globs).
    Probe(String),
    /// The target under the words a person reads: the first visible text
    /// equal to `text` in `area`, after each anchor in turn (paint order).
    Text {
        /// The exact words.
        text: String,
        /// Anchor texts it comes after, each after the one before.
        after: Vec<String>,
        /// Where to look.
        area: Option<Area>,
    },
}

impl std::fmt::Display for Pick {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Probe(probe) => f.write_str(probe),
            Self::Text { text, after, area } => {
                write!(f, "\"{text}\"")?;
                if !after.is_empty() {
                    f.write_str(" after")?;
                    for anchor in after {
                        write!(f, " \"{anchor}\"")?;
                    }
                }
                if let Some(area) = area {
                    write!(f, " in {}", area.name())?;
                }
                Ok(())
            }
        }
    }
}

/// One assertion on a settled checkpoint frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Assert {
    /// The route, exactly (harness route words, resolved on the fixture).
    Route(String),
    /// The route as exact words matches this glob (`*` any run): the
    /// production machine's route check, which needs no fixture to resolve.
    RouteLike(String),
    /// Each string is on screen (in the area), exactly.
    Text(Vec<String>, Option<Area>),
    /// Some visible text (in the area) is exactly matched by the glob
    /// (`"* declarations from * of * files"`): the evidence quotes it.
    Like(String, Option<Area>),
    /// No visible text (in the area) is matched by the glob.
    Unlike(String, Option<Area>),
    /// The area's pixels, averaged, read dark or light: the theme as the
    /// frame shows it, not as the model says.
    Ground(Tone, Option<Area>),
    /// Each string was on screen (in the area) in some frame drawn since the
    /// previous checkpoint: what a person saw while waiting.
    Saw(Vec<String>, Option<Area>),
    /// The strings are on screen in this paint order.
    Order(Vec<String>, Option<Area>),
    /// None of the strings is on screen.
    Absent(Vec<String>, Option<Area>),
    /// Some visual row reads the string (whitespace aside): words set as
    /// separate links still read as one line.
    Line(Vec<String>, Option<Area>),
    /// The pick is on screen and is a link (inside a published target).
    Link(Pick),
    /// The one focused target is this pick.
    Focus(Pick),
    /// The one focused target is the one this route was left by (a click
    /// here, then a back): the focus came back with the route.
    FocusRestored,
    /// What the app is doing (`state zone "shelf"`, `state held "2"`,
    /// `state clipboard "nudox://*"`): one of the words
    /// `look::state_words` reads, matched by the glob.
    State {
        /// Which: zone, focus, ask, peek, hints, hand, zen, shelf, drawer,
        /// overlay, held, text, theme, clipboard.
        key: String,
        /// Its expected words (`*` any run).
        glob: String,
    },
    /// The text is on screen (in the area) set at least this tall: its line
    /// height in px, so a text scale is seen, not read from the settings.
    Size {
        /// The words.
        text: String,
        /// The least line height, in px.
        at_least: f32,
        /// Where.
        area: Option<Area>,
    },
    /// A budget holds.
    Budget {
        /// Which.
        what: Budget,
        /// The limit in ms.
        limit_ms: f64,
    },
}

/// How a ground reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    /// Mean luminance under 0.35.
    Dark,
    /// Mean luminance over 0.65.
    Light,
}

/// A measured budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Budget {
    /// Last activation to the first complete frame.
    PageOpen,
    /// The same, named for search.
    Search,
    /// Last activation to the settle (virtual).
    Flight,
    /// p95 draw time over every frame so far.
    FrameP95,
}

impl Budget {
    /// The script spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PageOpen => "page-open",
            Self::Search => "search",
            Self::Flight => "flight",
            Self::FrameP95 => "frame-p95",
        }
    }
}

/// A token: a quoted string or a bare word.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Tok {
    Str(String),
    Word(String),
}

pub(super) fn tokens(text: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut chars = text.trim().chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        match chars.next() {
            None => break,
            Some('"') => {
                let mut value = String::new();
                loop {
                    match chars.next() {
                        None => return Err("an unterminated string".to_owned()),
                        Some('\\') => match chars.next() {
                            Some('n') => value.push('\n'),
                            Some(other) => value.push(other),
                            None => return Err("an unterminated string".to_owned()),
                        },
                        Some('"') => break,
                        Some(other) => value.push(other),
                    }
                }
                out.push(Tok::Str(value));
            }
            Some(first) => {
                let mut word = first.to_string();
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() {
                        break;
                    }
                    word.push(next);
                    chars.next();
                }
                out.push(Tok::Word(word));
            }
        }
    }
    Ok(out)
}

/// `"a" "b" [in AREA]`: at least one string, then an optional area.
fn strings_in(text: &str) -> Result<(Vec<String>, Option<Area>), String> {
    let toks = tokens(text)?;
    let mut list = Vec::new();
    let mut iter = toks.into_iter().peekable();
    while let Some(Tok::Str(value)) = iter.peek().cloned() {
        list.push(value);
        iter.next();
    }
    if list.is_empty() {
        return Err("expected at least one quoted string".to_owned());
    }
    let area = match (iter.next(), iter.next(), iter.next()) {
        (None, _, _) => None,
        (Some(Tok::Word(word)), Some(Tok::Word(area)), None) if word == "in" => Some(Area::parse(&area)?),
        _ => return Err("after the strings: nothing, or `in AREA`".to_owned()),
    };
    Ok((list, area))
}

/// `PROBE` or `"TEXT" [after "ANCHOR"…] [in AREA]`.
fn pick(text: &str) -> Result<Pick, String> {
    let toks = tokens(text)?;
    match toks.as_slice() {
        [Tok::Word(probe)] => Ok(Pick::Probe(probe.clone())),
        [Tok::Str(words), rest @ ..] => {
            let (mut after, mut area) = (Vec::new(), None);
            let mut rest = rest;
            loop {
                match rest {
                    [] => break,
                    [Tok::Word(key), Tok::Str(_), ..] if key == "after" && after.is_empty() => {
                        let mut tail = &rest[1..];
                        while let [Tok::Str(anchor), next @ ..] = tail {
                            after.push(anchor.clone());
                            tail = next;
                        }
                        rest = tail;
                    }
                    [Tok::Word(key), Tok::Word(name), tail @ ..] if key == "in" && area.is_none() => {
                        area = Some(Area::parse(name)?);
                        rest = tail;
                    }
                    _ => return Err("after the text: `after \"ANCHOR\"…` and/or `in AREA`".to_owned()),
                }
            }
            Ok(Pick::Text {
                text: words.clone(),
                after,
                area,
            })
        }
        _ => Err("expected a probe id or a quoted text".to_owned()),
    }
}

/// Splits `line` at the first ` else ` outside quotes.
pub(super) fn split_else(line: &str) -> (&str, Option<&str>) {
    let mut quoted = false;
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if quoted => index += 1,
            b'"' => quoted = !quoted,
            b' ' if !quoted && line[index..].starts_with(" else ") => {
                return (line[..index].trim_end(), Some(line[index + 6..].trim()));
            }
            _ => {}
        }
        index += 1;
    }
    (line, None)
}

/// Untimed script acts (`key cmd-k`), all at one instant.
fn acts(text: &str) -> Result<Vec<Act>, String> {
    let script = Script::parse(text).map_err(|error| error.to_string())?;
    if script.events.is_empty() {
        return Err("no act".to_owned());
    }
    if script.events.iter().any(|event| event.at_ms != 0) {
        return Err("journey acts are untimed (`@T`/`+T`): use `wait MS` or `settle`".to_owned());
    }
    if script.events.iter().any(|event| matches!(event.act, Act::Drag { .. })) {
        return Err("`drag` is not a journey act yet".to_owned());
    }
    for event in &script.events {
        if let Act::Route { target } = &event.act {
            route::parse(target)?;
        }
    }
    Ok(script.events.into_iter().map(|event| event.act).collect())
}

/// Whether an act injects a product intent instead of reaching the window
/// as a person's input (`route`, and the settings acts): only a detour may.
pub(super) fn injects(act: &Act) -> bool {
    act.is_setting()
}

fn budget(rest: &str) -> Result<Assert, String> {
    let words = rest.split_whitespace().collect::<Vec<_>>();
    let (what, limit) = match words.as_slice() {
        [what, "<=", limit] => (*what, *limit),
        _ => return Err(format!("`budget {rest}`: expected `budget KIND <= N ms`")),
    };
    let what = match what {
        "page-open" => Budget::PageOpen,
        "search" => Budget::Search,
        "flight" => Budget::Flight,
        "frame-p95" => Budget::FrameP95,
        other => {
            return Err(format!(
                "`{other}` is not a budget (page-open, search, flight, frame-p95)"
            ));
        }
    };
    let limit_ms = limit
        .trim_end_matches("ms")
        .parse::<f64>()
        .map_err(|_| format!("`{limit}` is not a time in ms"))?;
    Ok(Assert::Budget { what, limit_ms })
}

/// `size "WORDS" >= N [in AREA]`.
fn size(rest: &str) -> Result<Assert, String> {
    let usage = || format!("`size {rest}`: expected `size \"WORDS\" >= N [in AREA]`");
    let toks = tokens(rest)?;
    let (text, least, area) = match toks.as_slice() {
        [Tok::Str(text), Tok::Word(op), Tok::Word(least)] if op == ">=" => (text, least, None),
        [Tok::Str(text), Tok::Word(op), Tok::Word(least), Tok::Word(key), Tok::Word(area)] if op == ">=" && key == "in" => {
            (text, least, Some(Area::parse(area)?))
        }
        _ => return Err(usage()),
    };
    let at_least = least.trim_end_matches("px").parse::<f32>().map_err(|_| usage())?;
    Ok(Assert::Size { text: text.clone(), at_least, area })
}

/// One indented assert line.
pub(super) fn assertion(line: &str) -> Result<Assert, String> {
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    match verb {
        "route" if rest.starts_with("like ") => {
            let toks = tokens(&rest[5..])?;
            match toks.as_slice() {
                [Tok::Str(glob)] => Ok(Assert::RouteLike(glob.clone())),
                _ => Err("`route like \"GLOB\"`: one quoted glob".to_owned()),
            }
        }
        "route" if !rest.is_empty() => {
            route::parse(rest)?;
            Ok(Assert::Route(rest.to_owned()))
        }
        "text" => strings_in(rest).map(|(list, area)| Assert::Text(list, area)),
        "saw" => strings_in(rest).map(|(list, area)| Assert::Saw(list, area)),
        "ground" => {
            let (tone, area) = rest.split_once(" in ").map_or((rest, None), |(tone, area)| (tone.trim(), Some(area.trim())));
            let tone = match tone {
                "dark" => Tone::Dark,
                "light" => Tone::Light,
                other => return Err(format!("`ground {other}`: `ground dark|light [in AREA]`")),
            };
            Ok(Assert::Ground(tone, area.map(Area::parse).transpose()?))
        }
        "like" | "unlike" => {
            let (list, area) = strings_in(rest)?;
            let [glob] = list.as_slice() else {
                return Err(format!("`{verb} \"GLOB\" [in AREA]`: one glob"));
            };
            Ok(if verb == "like" { Assert::Like(glob.clone(), area) } else { Assert::Unlike(glob.clone(), area) })
        }
        "order" => {
            let (list, area) = strings_in(rest)?;
            if list.len() < 2 {
                return Err("`order` needs at least two strings".to_owned());
            }
            Ok(Assert::Order(list, area))
        }
        "absent" => strings_in(rest).map(|(list, area)| Assert::Absent(list, area)),
        "line" => strings_in(rest).map(|(list, area)| Assert::Line(list, area)),
        "focus" if rest == "restored" => Ok(Assert::FocusRestored),
        "focus" if !rest.is_empty() => pick(rest).map(Assert::Focus),
        "link" if !rest.is_empty() => pick(rest).map(Assert::Link),
        "budget" => budget(rest),
        "size" => size(rest),
        "state" => {
            const KEYS: [&str; 14] = ["zone", "focus", "ask", "peek", "hints", "hand", "zen", "shelf", "drawer", "overlay", "held", "text", "theme", "clipboard"];
            let toks = tokens(rest)?;
            match toks.as_slice() {
                [Tok::Word(key), Tok::Str(glob)] if KEYS.contains(&key.as_str()) => Ok(Assert::State { key: key.clone(), glob: glob.clone() }),
                _ => Err(format!("`state {rest}`: expected `state KEY \"GLOB\"`, KEY one of {}", KEYS.join(", "))),
            }
        }
        other => Err(format!(
            "`{other}` is not an assert (route, route like, text, like, unlike, saw, ground, order, absent, line, link, focus, budget, size, state)"
        )),
    }
}

/// `20m`, `90s`, `1500ms`.
fn duration(word: &str) -> Result<Duration, String> {
    let (number, unit) = word
        .find(|c: char| !c.is_ascii_digit())
        .map_or((word, ""), |at| word.split_at(at));
    let value = number.parse::<u64>().map_err(|_| format!("`{word}` is not a duration (20m, 90s, 1500ms)"))?;
    match unit {
        "ms" => Ok(Duration::from_millis(value)),
        "s" => Ok(Duration::from_secs(value)),
        "m" => Ok(Duration::from_secs(value * 60)),
        _ => Err(format!("`{word}` is not a duration (20m, 90s, 1500ms)")),
    }
}

/// `await text|absent "S"… [in AREA] within DURATION`.
fn await_step(rest: &str) -> Result<StepKind, String> {
    let (condition, within) = rest
        .rsplit_once(" within ")
        .ok_or_else(|| "`await text|absent \"S\"… [in AREA] within DURATION`".to_owned())?;
    let (verb, strings) = condition.trim().split_once(' ').unwrap_or((condition.trim(), ""));
    let (list, area) = strings_in(strings)?;
    let until = match verb {
        "text" => Until::Text(list, area),
        "absent" => Until::Absent(list, area),
        "like" => match list.as_slice() {
            [glob] => Until::Like(glob.clone(), area),
            _ => return Err("`await like \"GLOB\"`: one glob".to_owned()),
        },
        other => return Err(format!("`await {other}`: await `text`, `absent` or `like`")),
    };
    Ok(StepKind::Await { until, within: duration(within.trim())? })
}

/// A path a journey names: repo-relative, and it must exist.
pub(super) fn repo_path(word: &str) -> Result<PathBuf, String> {
    let path = super::super::repo().join(word);
    path.canonicalize().map_err(|error| format!("`{word}` is not a path under the repository: {error}"))
}

/// One step line (not a header, not `do`, not `needs`).
pub(super) fn step_kind(text: &str) -> Result<StepKind, String> {
    let (verb, rest) = text.split_once(' ').unwrap_or((text, ""));
    let rest = rest.trim();
    match verb {
        "settle" if rest.is_empty() => Ok(StepKind::Settle),
        "restart" if rest.is_empty() => Ok(StepKind::Restart),
        "wait" => rest
            .trim_end_matches("ms")
            .parse::<u64>()
            .map(StepKind::Wait)
            .map_err(|_| format!("`wait {rest}`: expected a time in ms")),
        "await" => await_step(rest),
        "answer-picker" => match rest {
            "cancel" => Ok(StepKind::AnswerPicker(PickerAnswer::Cancel)),
            "" => Err("`answer-picker PATH…|cancel`".to_owned()),
            paths => paths
                .split_whitespace()
                .map(repo_path)
                .collect::<Result<Vec<_>, _>>()
                .map(|folders| StepKind::AnswerPicker(PickerAnswer::Folders(folders))),
        },
        "hover" | "click" => match acts(text) {
            Ok(acts) => Ok(StepKind::Acts(acts)),
            Err(_) => pick(rest).map(|pick| StepKind::Pointer {
                click: verb == "click",
                pick,
            }),
        },
        _ => acts(text).map(StepKind::Acts),
    }
}

/// `*` matches any run of characters; everything else is literal.
#[must_use]
pub(super) fn glob(pattern: &str, key: &str) -> bool {
    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == key;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if key.len() < first.len() + last.len() || !key.starts_with(first) || !key[first.len()..].ends_with(last) {
        return false;
    }
    let mut rest = &key[first.len()..key.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{Area, Assert, Pick, PickerAnswer, StepKind, Until, assertion, glob, pick, split_else, step_kind, strings_in};
    use std::time::Duration;

    #[test]
    fn globs_match_whole_keys() {
        assert!(glob("orbit-package-*toml_pin", "orbit-package-/a/b/toml_pin"));
        assert!(!glob("orbit-package-*toml_pin", "orbit-package-/a/b/toml_pin/x"));
        assert!(glob("pkg-*::from_str*", "pkg-rust:toml::from_str#12"));
        assert!(glob("resume", "resume"));
        assert!(!glob("resume", "resume-2"));
        // A key shorter than the glob's fixed ends never matches (no slice panic).
        assert!(!glob("abc*abc", "abc"));
    }

    #[test]
    fn strings_picks_and_detours_parse() {
        assert_eq!(
            strings_in(r#""a" "b \"c\"" in reader"#),
            Ok((vec!["a".to_owned(), "b \"c\"".to_owned()], Some(Area::Reader)))
        );
        assert!(strings_in("a").is_err());
        assert!(strings_in(r#""a" in nowhere"#).is_err());
        assert_eq!(
            pick(r#""ensure_locald" after "How it fails" "Spawn" in reader"#),
            Ok(Pick::Text {
                text: "ensure_locald".to_owned(),
                after: vec!["How it fails".to_owned(), "Spawn".to_owned()],
                area: Some(Area::Reader),
            })
        );
        assert_eq!(pick("orbit-package-*"), Ok(Pick::Probe("orbit-package-*".to_owned())));
        assert_eq!(split_else("click x else route orbit"), ("click x", Some("route orbit")));
        assert_eq!(split_else(r#"type "a else b""#), (r#"type "a else b""#, None));
    }

    #[test]
    fn production_steps_and_asserts_parse() {
        let StepKind::Await { until, within } = step_kind(r#"await absent "indexing" in reader within 20m"#).expect("await") else {
            panic!("not an await");
        };
        assert_eq!(until, Until::Absent(vec!["indexing".to_owned()], Some(Area::Reader)));
        assert_eq!(within, Duration::from_secs(1200));
        assert!(step_kind(r#"await text "x" within soon"#).is_err());
        assert!(step_kind(r#"await "x" within 2m"#).is_err());
        assert!(matches!(step_kind("restart"), Ok(StepKind::Restart)));
        assert!(matches!(step_kind("answer-picker cancel"), Ok(StepKind::AnswerPicker(PickerAnswer::Cancel))));
        let Ok(StepKind::AnswerPicker(PickerAnswer::Folders(folders))) = step_kind("answer-picker frontends/rust/fixtures/toml_pin") else {
            panic!("a repo folder answers the picker");
        };
        assert!(folders[0].is_absolute() && folders[0].ends_with("frontends/rust/fixtures/toml_pin"));
        assert!(step_kind("answer-picker no/such/folder").is_err(), "a folder that does not exist is refused at parse");
        assert_eq!(assertion(r#"route like "package *toml_pin""#), Ok(Assert::RouteLike("package *toml_pin".to_owned())));
        assert_eq!(assertion(r#"saw "Compiling" in reader"#), Ok(Assert::Saw(vec!["Compiling".to_owned()], Some(Area::Reader))));
        assert_eq!(assertion(r#"like "* declarations from *" in reader"#), Ok(Assert::Like("* declarations from *".to_owned(), Some(Area::Reader))));
        assert_eq!(assertion(r#"unlike "0 declarations *""#), Ok(Assert::Unlike("0 declarations *".to_owned(), None)));
        assert!(assertion(r#"like "a" "b""#).is_err(), "one glob");
        assert_eq!(assertion("ground dark in reader"), Ok(Assert::Ground(super::Tone::Dark, Some(Area::Reader))));
        assert_eq!(assertion("ground light"), Ok(Assert::Ground(super::Tone::Light, None)));
        assert!(assertion("ground grey").is_err());
    }
}
