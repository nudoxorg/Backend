//! The journey script: acts (the `storm --replay` language, untimed), picks
//! of what a person points at, and checkpoints of content asserts.

use super::super::route;
use backend_gui_harness::{Act, Script};
use std::path::{Path, PathBuf};

/// A parsed journey.
#[derive(Clone, Debug)]
pub struct Journey {
    /// The file's stem (`J1`).
    pub name: String,
    /// Where it was read from.
    pub source: PathBuf,
    /// The window's logical size.
    pub size: (u32, u32),
    /// The boot route, in harness route words.
    pub start: String,
    /// The steps in order.
    pub steps: Vec<Step>,
}

/// One step with its line.
#[derive(Clone, Debug)]
pub struct Step {
    /// 1-based line in the script.
    pub line: usize,
    /// The line as written.
    pub text: String,
    /// What it does.
    pub kind: StepKind,
    /// The detour taken when the step cannot be done.
    pub otherwise: Option<Vec<Act>>,
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
    /// equal to `text` (after the first `after`, when given) in `area`.
    Text {
        /// The exact words.
        text: String,
        /// An anchor text it comes after, in paint order.
        after: Option<String>,
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
                if let Some(after) = after {
                    write!(f, " after \"{after}\"")?;
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
    /// The route, exactly (harness route words).
    Route(String),
    /// Each string is on screen (in the area), exactly.
    Text(Vec<String>, Option<Area>),
    /// The strings are on screen in this paint order.
    Order(Vec<String>, Option<Area>),
    /// None of the strings is on screen.
    Absent(Vec<String>, Option<Area>),
    /// Some visual row reads the string (whitespace aside): words set as
    /// separate links still read as one line.
    Line(Vec<String>, Option<Area>),
    /// The one focused target is this pick.
    Focus(Pick),
    /// A budget holds.
    Budget {
        /// Which.
        what: Budget,
        /// The limit in ms.
        limit_ms: f64,
    },
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
enum Tok {
    Str(String),
    Word(String),
}

fn tokens(text: &str) -> Result<Vec<Tok>, String> {
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

/// `PROBE` or `"TEXT" [after "ANCHOR"] [in AREA]`.
fn pick(text: &str) -> Result<Pick, String> {
    let toks = tokens(text)?;
    match toks.as_slice() {
        [Tok::Word(probe)] => Ok(Pick::Probe(probe.clone())),
        [Tok::Str(words), rest @ ..] => {
            let (mut after, mut area) = (None, None);
            let mut rest = rest;
            loop {
                match rest {
                    [] => break,
                    [Tok::Word(key), Tok::Str(anchor), tail @ ..] if key == "after" && after.is_none() => {
                        after = Some(anchor.clone());
                        rest = tail;
                    }
                    [Tok::Word(key), Tok::Word(name), tail @ ..] if key == "in" && area.is_none() => {
                        area = Some(Area::parse(name)?);
                        rest = tail;
                    }
                    _ => return Err("after the text: `after \"ANCHOR\"` and/or `in AREA`".to_owned()),
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
fn split_else(line: &str) -> (&str, Option<&str>) {
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

fn assertion(line: &str) -> Result<Assert, String> {
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    match verb {
        "route" if !rest.is_empty() => {
            route::parse(rest)?;
            Ok(Assert::Route(rest.to_owned()))
        }
        "text" => strings_in(rest).map(|(list, area)| Assert::Text(list, area)),
        "order" => {
            let (list, area) = strings_in(rest)?;
            if list.len() < 2 {
                return Err("`order` needs at least two strings".to_owned());
            }
            Ok(Assert::Order(list, area))
        }
        "absent" => strings_in(rest).map(|(list, area)| Assert::Absent(list, area)),
        "line" => strings_in(rest).map(|(list, area)| Assert::Line(list, area)),
        "focus" if !rest.is_empty() => pick(rest).map(Assert::Focus),
        "budget" => budget(rest),
        other => Err(format!(
            "`{other}` is not an assert (route, text, order, absent, line, focus, budget)"
        )),
    }
}

fn step_kind(text: &str) -> Result<StepKind, String> {
    let (verb, rest) = text.split_once(' ').unwrap_or((text, ""));
    let rest = rest.trim();
    match verb {
        "settle" if rest.is_empty() => Ok(StepKind::Settle),
        "wait" => rest
            .trim_end_matches("ms")
            .parse::<u64>()
            .map(StepKind::Wait)
            .map_err(|_| format!("`wait {rest}`: expected a time in ms")),
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

impl Journey {
    /// Parses a journey script.
    ///
    /// # Errors
    /// The first line that does not parse, with its number.
    pub fn parse(name: &str, source: &Path, text: &str) -> Result<Self, String> {
        let mut journey = Self {
            name: name.to_owned(),
            source: source.to_path_buf(),
            size: (1440, 900),
            start: "orbit".to_owned(),
            steps: Vec::new(),
        };
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let fail =
                |message: String| format!("{}:{line}: {message}\n    {raw}", source.display());
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if raw.starts_with(char::is_whitespace) {
                let Some(Step {
                    kind: StepKind::Check { asserts, .. },
                    ..
                }) = journey.steps.last_mut()
                else {
                    return Err(fail("an indented assert belongs under a `check NAME`".to_owned()));
                };
                asserts.push(assertion(trimmed).map_err(fail)?);
                continue;
            }
            let (verb, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
            match verb {
                "size" => {
                    let (width, height) = rest
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
                        .ok_or_else(|| fail(format!("`{rest}` is not WxH")))?;
                    journey.size = (width, height);
                }
                "start" => {
                    route::parse(rest).map_err(fail)?;
                    rest.trim().clone_into(&mut journey.start);
                }
                "check" => {
                    if rest.is_empty() {
                        return Err(fail("`check` needs a name".to_owned()));
                    }
                    journey.steps.push(Step {
                        line,
                        text: trimmed.to_owned(),
                        kind: StepKind::Check {
                            name: rest.to_owned(),
                            asserts: Vec::new(),
                        },
                        otherwise: None,
                    });
                }
                _ => {
                    let (main, otherwise) = split_else(trimmed);
                    let kind = step_kind(main).map_err(fail)?;
                    let otherwise = otherwise.map(acts).transpose().map_err(fail)?;
                    journey.steps.push(Step {
                        line,
                        text: trimmed.to_owned(),
                        kind,
                        otherwise,
                    });
                }
            }
        }
        if !journey
            .steps
            .iter()
            .any(|step| matches!(step.kind, StepKind::Check { .. }))
        {
            return Err(format!("{}: a journey needs at least one `check`", source.display()));
        }
        Ok(journey)
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
    if !key.starts_with(first) || !key[first.len()..].ends_with(last) {
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
    use super::{Area, Assert, Budget, Journey, Pick, StepKind, glob, pick, split_else, strings_in};
    use std::path::Path;

    #[test]
    fn globs_match_whole_keys() {
        assert!(glob("orbit-package-*toml_pin", "orbit-package-/a/b/toml_pin"));
        assert!(!glob("orbit-package-*toml_pin", "orbit-package-/a/b/toml_pin/x"));
        assert!(glob("pkg-*::from_str*", "pkg-rust:toml::from_str#12"));
        assert!(glob("resume", "resume"));
        assert!(!glob("resume", "resume-2"));
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
            pick(r#""ensure_locald" after "Spawn" in reader"#),
            Ok(Pick::Text {
                text: "ensure_locald".to_owned(),
                after: Some("Spawn".to_owned()),
                area: Some(Area::Reader),
            })
        );
        assert_eq!(pick("orbit-package-*"), Ok(Pick::Probe("orbit-package-*".to_owned())));
        assert_eq!(split_else("click x else route orbit"), ("click x", Some("route orbit")));
        assert_eq!(split_else(r#"type "a else b""#), (r#"type "a else b""#, None));
    }

    #[test]
    fn a_journey_parses_steps_and_asserts() {
        let text = "size 1440x900\nstart orbit\n# a comment\nkey cmd-k\ntype \"toml Value\"\nclick orbit-package-* else route orbit\nclick \"Spawn\" in reader\nsettle\nwait 120\ncheck home\n  route orbit\n  text \"a\" \"b\" in reader\n  order \"a\" \"b\"\n  line \"Io, Spawn\"\n  focus x*\n  budget page-open <= 120ms\n";
        let journey = Journey::parse("J0", Path::new("J0.journey"), text).expect("parses");
        assert_eq!(journey.steps.len(), 7);
        assert!(matches!(journey.steps[2].kind, StepKind::Pointer { click: true, pick: Pick::Probe(_) }));
        assert!(journey.steps[2].otherwise.is_some());
        assert!(matches!(journey.steps[3].kind, StepKind::Pointer { pick: Pick::Text { .. }, .. }));
        let StepKind::Check { asserts, .. } = &journey.steps[6].kind else {
            panic!("the last step is a check");
        };
        assert_eq!(asserts[0], Assert::Route("orbit".to_owned()));
        assert_eq!(asserts[1], Assert::Text(vec!["a".to_owned(), "b".to_owned()], Some(Area::Reader)));
        assert_eq!(
            asserts[5],
            Assert::Budget {
                what: Budget::PageOpen,
                limit_ms: 120.0
            }
        );
        assert!(Journey::parse("J0", Path::new("J0"), "  text \"a\"\ncheck x\n").is_err());
        assert!(Journey::parse("J0", Path::new("J0"), "key j @100\ncheck x\n").is_err());
        assert!(Journey::parse("J0", Path::new("J0"), "route elsewhere\ncheck x\n").is_err());
    }
}
