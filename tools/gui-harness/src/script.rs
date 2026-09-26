//! Timed input scripts: what a person (or a storm) does to a window, each act
//! at a virtual time.
//!
//! ```text
//! move 420,310 @0; key cmd-k @200; hold alt @400; release alt @700
//! ```
//!
//! Statements are separated by `;` or newlines; `#` starts a comment. Each
//! statement is a verb, its arguments, and an optional time: `@T` is absolute
//! virtual ms after the first frame, `+T` is relative to the previous
//! statement, and no time means "at the same instant as the previous
//! statement" (0 for the first). Several acts at one instant are delivered in
//! order against the same painted frame, the way a burst of platform events
//! lands between two vsyncs.
//!
//! | Verb | Arguments | Meaning |
//! |---|---|---|
//! | `move` / `hover` | `X,Y` | pointer moves (the pressed button, if any, drags) |
//! | `down` / `up` | `[left\|right\|middle] X,Y` | a button goes down / up |
//! | `click` | `[button] X,Y` | down and up at one instant |
//! | `scroll` | `X,Y DX,DY` | a pixel-precise wheel delta at a position |
//! | `wheel-zoom` | `X,Y FACTOR` | a pinch (the ctrl-wheel/trackpad zoom gesture) about a point; `1.25` zooms in 25 %, `0.8` out |
//! | `drag` | `[button] X,Y -> X2,Y2 over MS` | button down, one interpolated move per frame period, button up at the end |
//! | `route` | `KIND ARGS…` | the product navigates (`route symbol present::glyph::RelationLabel view=graph at=0.3.0`); the product adapter owns the words |
//! | `leave` | | the pointer leaves the window |
//! | `key` | `CHORD…` | GPUI keystrokes (`cmd-k`, `shift-tab`, `escape`); held modifiers are added |
//! | `type` | `"TEXT"` | characters through the key path |
//! | `hold` / `release` | `cmd+alt…` or `all` | modifiers go down / up (drives the facet `Reveal`) |
//! | `resize` | `WxH` | the window's logical size |
//! | `text-scale` | `PCT` | the product's text scale (85–200) |
//! | `density` | `comfortable\|compact\|dense` | the product's density |
//! | `theme` | `abyss\|glacier` | the product's appearance |
//! | `contrast` | `normal\|high` | the product's contrast |
//! | `motion` | `on\|off` | reduced motion off / on |
//!
//! The same script always produces the same frames: a script is data, and
//! [`Script::to_string`] prints the canonical form (absolute times, one act
//! per line) that [`Script::parse`] reads back unchanged.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A mouse button.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Button {
    /// The primary button.
    Left,
    /// The secondary button.
    Right,
    /// The wheel button.
    Middle,
}

impl Button {
    /// The script spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "middle" => Some(Self::Middle),
            _ => None,
        }
    }
}

/// A set of modifier keys.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Mods {
    /// ⌘ (platform/command).
    pub cmd: bool,
    /// ⌥ (alt/option).
    pub alt: bool,
    /// ⇧.
    pub shift: bool,
    /// ⌃.
    pub ctrl: bool,
}

impl Mods {
    /// Every modifier.
    pub const ALL: Self = Self {
        cmd: true,
        alt: true,
        shift: true,
        ctrl: true,
    };

    /// Whether no modifier is in the set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !(self.cmd || self.alt || self.shift || self.ctrl)
    }

    /// `self` with every key of `other` added.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self {
            cmd: self.cmd || other.cmd,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
            ctrl: self.ctrl || other.ctrl,
        }
    }

    /// `self` with every key of `other` removed.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self {
            cmd: self.cmd && !other.cmd,
            alt: self.alt && !other.alt,
            shift: self.shift && !other.shift,
            ctrl: self.ctrl && !other.ctrl,
        }
    }

    fn parse(word: &str) -> Result<Self, String> {
        if word == "all" {
            return Ok(Self::ALL);
        }
        let mut mods = Self::default();
        for part in word.split('+') {
            match part {
                "cmd" | "super" | "platform" => mods.cmd = true,
                "alt" | "option" | "opt" => mods.alt = true,
                "shift" => mods.shift = true,
                "ctrl" | "control" => mods.ctrl = true,
                other => return Err(format!("`{other}` is not a modifier (cmd, alt, shift, ctrl)")),
            }
        }
        Ok(mods)
    }
}

impl fmt::Display for Mods {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Self::ALL {
            return f.write_str("all");
        }
        let names = [
            (self.cmd, "cmd"),
            (self.alt, "alt"),
            (self.shift, "shift"),
            (self.ctrl, "ctrl"),
        ];
        let mut first = true;
        for (on, name) in names {
            if on {
                if !first {
                    f.write_str("+")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

/// One act.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "act", rename_all = "kebab-case")]
pub enum Act {
    /// The pointer moves to a logical position.
    Move {
        /// Logical x.
        x: f32,
        /// Logical y.
        y: f32,
    },
    /// A button goes down.
    Down {
        /// Logical x.
        x: f32,
        /// Logical y.
        y: f32,
        /// Which button.
        button: Button,
    },
    /// A button goes up.
    Up {
        /// Logical x.
        x: f32,
        /// Logical y.
        y: f32,
        /// Which button.
        button: Button,
    },
    /// A button goes down and up.
    Click {
        /// Logical x.
        x: f32,
        /// Logical y.
        y: f32,
        /// Which button.
        button: Button,
    },
    /// A wheel delta at a position.
    Scroll {
        /// Logical x.
        x: f32,
        /// Logical y.
        y: f32,
        /// Horizontal delta in px.
        dx: f32,
        /// Vertical delta in px.
        dy: f32,
    },
    /// A pinch about a point: the zoom gesture a trackpad (or ctrl-wheel)
    /// produces.
    Zoom {
        /// Logical x.
        x: f32,
        /// Logical y.
        y: f32,
        /// Scale factor (1.25 = zoom in 25 %).
        factor: f32,
    },
    /// Button down at `from`, interpolated moves at the frame period, up at
    /// `to` after `over_ms`. Expanded by [`Script::expanded`] when played.
    Drag {
        /// Start x.
        from_x: f32,
        /// Start y.
        from_y: f32,
        /// End x.
        to_x: f32,
        /// End y.
        to_y: f32,
        /// Duration in virtual ms.
        over_ms: u64,
        /// Which button.
        button: Button,
    },
    /// The product navigates; the words after `route` belong to the
    /// product adapter.
    Route {
        /// `symbol present::glyph::RelationLabel view=graph at=0.3.0`.
        target: String,
    },
    /// The pointer leaves the window.
    Leave,
    /// One keystroke (GPUI spelling).
    Key {
        /// `cmd-k`, `escape`, `shift-tab`.
        chord: String,
    },
    /// Characters typed one by one.
    Type {
        /// The text.
        text: String,
    },
    /// Modifiers go down.
    Hold {
        /// Which.
        mods: Mods,
    },
    /// Modifiers go up.
    Release {
        /// Which.
        mods: Mods,
    },
    /// The window's logical size changes.
    Resize {
        /// Logical width.
        width: u32,
        /// Logical height.
        height: u32,
    },
    /// The product's text scale changes.
    TextScale {
        /// Percent (85–200).
        percent: u16,
    },
    /// The product's density changes.
    Density {
        /// `comfortable`, `compact`, `dense`.
        name: String,
    },
    /// The product's appearance changes.
    Theme {
        /// `abyss`, `glacier`.
        name: String,
    },
    /// The product's contrast changes.
    Contrast {
        /// `normal`, `high`.
        name: String,
    },
    /// Motion on (true) or reduced (false).
    Motion {
        /// Whether motion runs.
        on: bool,
    },
}

impl Act {
    /// Whether the act is delivered through the product adapter (the product
    /// owns the setting) rather than as a platform event.
    #[must_use]
    pub const fn is_setting(&self) -> bool {
        matches!(
            self,
            Self::Route { .. }
                | Self::TextScale { .. }
                | Self::Density { .. }
                | Self::Theme { .. }
                | Self::Contrast { .. }
                | Self::Motion { .. }
        )
    }

    /// Whether the act can change persistent product state (activation),
    /// as opposed to transient state (hover, held keys, window size).
    #[must_use]
    pub const fn is_activation(&self) -> bool {
        matches!(
            self,
            Self::Click { .. }
                | Self::Down { .. }
                | Self::Key { .. }
                | Self::Type { .. }
                | Self::Drag { .. }
                | Self::Route { .. }
        )
    }
}

fn number(value: f32) -> String {
    if value.fract() == 0.0 && value.abs() < 1e7 {
        format!("{value:.0}")
    } else {
        let text = format!("{value:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

fn number_fine(value: f32) -> String {
    let text = format!("{value:.4}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

impl fmt::Display for Act {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Move { x, y } => write!(f, "move {},{}", number(*x), number(*y)),
            Self::Down { x, y, button } => {
                write!(f, "down {} {},{}", button.name(), number(*x), number(*y))
            }
            Self::Up { x, y, button } => {
                write!(f, "up {} {},{}", button.name(), number(*x), number(*y))
            }
            Self::Click { x, y, button } => {
                write!(f, "click {} {},{}", button.name(), number(*x), number(*y))
            }
            Self::Scroll { x, y, dx, dy } => write!(
                f,
                "scroll {},{} {},{}",
                number(*x),
                number(*y),
                number(*dx),
                number(*dy)
            ),
            Self::Zoom { x, y, factor } => write!(
                f,
                "wheel-zoom {},{} {}",
                number(*x),
                number(*y),
                number_fine(*factor)
            ),
            Self::Drag {
                from_x,
                from_y,
                to_x,
                to_y,
                over_ms,
                button,
            } => write!(
                f,
                "drag {} {},{} -> {},{} over {over_ms}",
                button.name(),
                number(*from_x),
                number(*from_y),
                number(*to_x),
                number(*to_y)
            ),
            Self::Route { target } => write!(f, "route {target}"),
            Self::Leave => f.write_str("leave"),
            Self::Key { chord } => write!(f, "key {chord}"),
            Self::Type { text } => {
                f.write_str("type \"")?;
                for ch in text.chars() {
                    match ch {
                        '"' => f.write_str("\\\"")?,
                        '\\' => f.write_str("\\\\")?,
                        '\n' => f.write_str("\\n")?,
                        ch => write!(f, "{ch}")?,
                    }
                }
                f.write_str("\"")
            }
            Self::Hold { mods } => write!(f, "hold {mods}"),
            Self::Release { mods } => write!(f, "release {mods}"),
            Self::Resize { width, height } => write!(f, "resize {width}x{height}"),
            Self::TextScale { percent } => write!(f, "text-scale {percent}"),
            Self::Density { name } => write!(f, "density {name}"),
            Self::Theme { name } => write!(f, "theme {name}"),
            Self::Contrast { name } => write!(f, "contrast {name}"),
            Self::Motion { on } => write!(f, "motion {}", if *on { "on" } else { "off" }),
        }
    }
}

/// An act at a virtual time.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Event {
    /// Virtual ms after the first frame.
    pub at_ms: u64,
    /// What happens.
    pub act: Act,
}

/// A timed input script, ordered by time (stable within one instant).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Script {
    /// The events, ascending by `at_ms`.
    pub events: Vec<Event>,
}

/// A script that does not parse, with the statement that failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    /// 1-based statement number.
    pub statement: usize,
    /// The statement's text.
    pub text: String,
    /// What is wrong with it.
    pub message: String,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "input statement {} `{}`: {}",
            self.statement, self.text, self.message
        )
    }
}

impl std::error::Error for ScriptError {}

/// Splits on `;` and newlines outside double quotes, dropping `#` comments.
fn statements(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut comment = false;
    for ch in source.chars() {
        if comment {
            if ch == '\n' {
                comment = false;
                out.push(std::mem::take(&mut current));
            }
            continue;
        }
        if quoted {
            current.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                current.push(ch);
            }
            '#' => comment = true,
            ';' | '\n' => out.push(std::mem::take(&mut current)),
            ch => current.push(ch),
        }
    }
    out.push(current);
    out.into_iter()
        .map(|statement| statement.trim().to_owned())
        .filter(|statement| !statement.is_empty())
        .collect()
}

/// Splits a statement into words, keeping a quoted string as one word
/// (unescaped, without its quotes, marked by a leading `"`).
fn words(statement: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = statement.chars().peekable();
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }
        if ch == '"' {
            chars.next();
            let mut text = String::from("\"");
            let mut closed = false;
            while let Some(ch) = chars.next() {
                match ch {
                    '\\' => match chars.next() {
                        Some('n') => text.push('\n'),
                        Some(other) => text.push(other),
                        None => return Err("dangling escape".to_owned()),
                    },
                    '"' => {
                        closed = true;
                        break;
                    }
                    ch => text.push(ch),
                }
            }
            if !closed {
                return Err("unterminated string".to_owned());
            }
            out.push(text);
            continue;
        }
        let mut word = String::new();
        while let Some(&ch) = chars.peek() {
            if ch.is_whitespace() {
                break;
            }
            word.push(ch);
            chars.next();
        }
        out.push(word);
    }
    Ok(out)
}

fn pair(word: &str) -> Result<(f32, f32), String> {
    let Some((a, b)) = word.split_once(',') else {
        return Err(format!("`{word}` is not X,Y"));
    };
    let parse = |part: &str| {
        part.trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("`{part}` is not a number"))
    };
    Ok((parse(a)?, parse(b)?))
}

fn button_and_point(args: &[String]) -> Result<(Button, f32, f32), String> {
    match args {
        [point] => {
            let (x, y) = pair(point)?;
            Ok((Button::Left, x, y))
        }
        [button, point] => {
            let button = Button::parse(button)
                .ok_or_else(|| format!("`{button}` is not a button (left, right, middle)"))?;
            let (x, y) = pair(point)?;
            Ok((button, x, y))
        }
        _ => Err("expected [BUTTON] X,Y".to_owned()),
    }
}

fn one<'a>(args: &'a [String], what: &str) -> Result<&'a str, String> {
    match args {
        [arg] => Ok(arg.as_str()),
        _ => Err(format!("expected {what}")),
    }
}

fn choice(word: &str, options: &[&str]) -> Result<String, String> {
    if options.contains(&word) {
        Ok(word.to_owned())
    } else {
        Err(format!("`{word}`: expected one of {}", options.join(", ")))
    }
}

fn act(verb: &str, args: &[String]) -> Result<Vec<Act>, String> {
    Ok(match verb {
        "move" | "hover" => {
            let (x, y) = pair(one(args, "X,Y")?)?;
            vec![Act::Move { x, y }]
        }
        "down" => {
            let (button, x, y) = button_and_point(args)?;
            vec![Act::Down { x, y, button }]
        }
        "up" => {
            let (button, x, y) = button_and_point(args)?;
            vec![Act::Up { x, y, button }]
        }
        "click" => {
            let (button, x, y) = button_and_point(args)?;
            vec![Act::Click { x, y, button }]
        }
        "scroll" => match args {
            [point, delta] => {
                let (x, y) = pair(point)?;
                let (dx, dy) = pair(delta)?;
                vec![Act::Scroll { x, y, dx, dy }]
            }
            _ => return Err("expected X,Y DX,DY".to_owned()),
        },
        "wheel-zoom" | "zoom" => match args {
            [point, factor] => {
                let (x, y) = pair(point)?;
                let factor = factor
                    .parse::<f32>()
                    .ok()
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .ok_or_else(|| format!("`{factor}` is not a zoom factor above 0"))?;
                vec![Act::Zoom { x, y, factor }]
            }
            _ => return Err("expected X,Y FACTOR".to_owned()),
        },
        "drag" => {
            let (button, rest) = match args.first().and_then(|word| Button::parse(word)) {
                Some(button) => (button, &args[1..]),
                None => (Button::Left, args),
            };
            match rest {
                [from, arrow, to, over, duration] if arrow == "->" && over == "over" => {
                    let (from_x, from_y) = pair(from)?;
                    let (to_x, to_y) = pair(to)?;
                    let over_ms = duration
                        .trim_end_matches("ms")
                        .parse::<u64>()
                        .map_err(|_| format!("`{duration}` is not a duration in ms"))?;
                    vec![Act::Drag {
                        from_x,
                        from_y,
                        to_x,
                        to_y,
                        over_ms,
                        button,
                    }]
                }
                _ => return Err("expected [BUTTON] X,Y -> X2,Y2 over MS".to_owned()),
            }
        }
        "route" => {
            if args.is_empty() || args.iter().any(|arg| arg.starts_with('"')) {
                return Err("expected route KIND ARGS… (no quotes)".to_owned());
            }
            vec![Act::Route {
                target: args.join(" "),
            }]
        }
        "leave" => {
            if !args.is_empty() {
                return Err("leave takes no arguments".to_owned());
            }
            vec![Act::Leave]
        }
        "key" | "keys" => {
            if args.is_empty() {
                return Err("expected one or more chords".to_owned());
            }
            args.iter()
                .map(|chord| {
                    gpui::Keystroke::parse(chord)
                        .map_err(|error| format!("`{chord}`: {error}"))?;
                    Ok(Act::Key {
                        chord: chord.clone(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?
        }
        "type" => {
            let text = one(args, "\"TEXT\"")?;
            let Some(text) = text.strip_prefix('"') else {
                return Err("type needs a quoted string".to_owned());
            };
            vec![Act::Type {
                text: text.to_owned(),
            }]
        }
        "hold" => vec![Act::Hold {
            mods: Mods::parse(one(args, "MODIFIERS")?)?,
        }],
        "release" => vec![Act::Release {
            mods: Mods::parse(one(args, "MODIFIERS")?)?,
        }],
        "resize" => {
            let size = one(args, "WxH")?;
            let Some((width, height)) = size.split_once('x') else {
                return Err(format!("`{size}` is not WxH"));
            };
            let width = width
                .parse::<u32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("`{width}` is not a width"))?;
            let height = height
                .parse::<u32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("`{height}` is not a height"))?;
            vec![Act::Resize { width, height }]
        }
        "text-scale" => {
            let percent = one(args, "PERCENT")?;
            let percent = percent
                .trim_end_matches('%')
                .parse::<u16>()
                .ok()
                .filter(|value| (50..=300).contains(value))
                .ok_or_else(|| format!("`{percent}` is not a percent in 50..=300"))?;
            vec![Act::TextScale { percent }]
        }
        "density" => vec![Act::Density {
            name: choice(one(args, "DENSITY")?, &["comfortable", "compact", "dense"])?,
        }],
        "theme" => vec![Act::Theme {
            name: choice(one(args, "THEME")?, &["abyss", "glacier"])?,
        }],
        "contrast" => vec![Act::Contrast {
            name: choice(one(args, "CONTRAST")?, &["normal", "high"])?,
        }],
        "motion" => vec![Act::Motion {
            on: match one(args, "on|off")? {
                "on" => true,
                "off" => false,
                other => return Err(format!("`{other}`: expected on or off")),
            },
        }],
        other => return Err(format!("unknown verb `{other}`")),
    })
}

impl Script {
    /// An empty script.
    #[must_use]
    pub const fn new() -> Self {
        Self { events: Vec::new() }
    }

    /// Parses the script syntax described in the module docs.
    ///
    /// # Errors
    /// The first statement that does not parse.
    pub fn parse(source: &str) -> Result<Self, ScriptError> {
        let mut events = Vec::new();
        let mut previous = 0_u64;
        for (index, statement) in statements(source).into_iter().enumerate() {
            let fail = |message: String| ScriptError {
                statement: index + 1,
                text: statement.clone(),
                message,
            };
            let mut parts = words(&statement).map_err(&fail)?;
            let mut at = previous;
            if let Some(last) = parts.last()
                && !last.starts_with('"')
            {
                if let Some(time) = last.strip_prefix('@') {
                    at = time
                        .parse::<u64>()
                        .map_err(|_| fail(format!("`{last}` is not a time in ms")))?;
                    parts.pop();
                } else if let Some(delta) = last.strip_prefix('+') {
                    let delta = delta
                        .parse::<u64>()
                        .map_err(|_| fail(format!("`{last}` is not a delay in ms")))?;
                    at = previous.saturating_add(delta);
                    parts.pop();
                }
            }
            if at < previous {
                return Err(fail(format!(
                    "time {at} ms is before the previous statement ({previous} ms)"
                )));
            }
            let Some((verb, args)) = parts.split_first() else {
                return Err(fail("empty statement".to_owned()));
            };
            for act in act(verb, args).map_err(&fail)? {
                events.push(Event { at_ms: at, act });
            }
            previous = at;
        }
        Ok(Self { events })
    }

    /// Whether the script does nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// The time of the last event (0 for an empty script).
    #[must_use]
    pub fn end_ms(&self) -> u64 {
        self.events
            .iter()
            .map(|event| match event.act {
                Act::Drag { over_ms, .. } => event.at_ms + over_ms,
                _ => event.at_ms,
            })
            .max()
            .unwrap_or(0)
    }

    /// Adds an act at `at_ms`, after every event already at or before it.
    pub fn push(&mut self, at_ms: u64, act: Act) {
        let index = self.events.partition_point(|event| event.at_ms <= at_ms);
        self.events.insert(index, Event { at_ms, act });
    }

    /// The same script with every event `by` ms later.
    #[must_use]
    pub fn shifted(&self, by: u64) -> Self {
        Self {
            events: self
                .events
                .iter()
                .map(|event| Event {
                    at_ms: event.at_ms.saturating_add(by),
                    act: event.act.clone(),
                })
                .collect(),
        }
    }

    /// `self` followed by `other` (merged by time; `self` first within one
    /// instant).
    #[must_use]
    pub fn then(&self, other: &Self) -> Self {
        let mut out = self.clone();
        for event in &other.events {
            out.push(event.at_ms, event.act.clone());
        }
        out
    }

    /// The script as the platform receives it: every `drag` expanded into
    /// a button down, one move per `frame_ms` (16 when 0), and a button up
    /// at its end, merged in time order with the rest.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn expanded(&self, frame_ms: u64) -> Self {
        let step = if frame_ms == 0 { 16 } else { frame_ms };
        let mut out = Self::new();
        for event in &self.events {
            let Act::Drag {
                from_x,
                from_y,
                to_x,
                to_y,
                over_ms,
                button,
            } = event.act
            else {
                out.push(event.at_ms, event.act.clone());
                continue;
            };
            let start = event.at_ms;
            out.push(
                start,
                Act::Down {
                    x: from_x,
                    y: from_y,
                    button,
                },
            );
            let mut at = start + step;
            while at < start + over_ms {
                let t = (at - start) as f32 / over_ms.max(1) as f32;
                out.push(
                    at,
                    Act::Move {
                        x: (from_x + (to_x - from_x) * t).round(),
                        y: (from_y + (to_y - from_y) * t).round(),
                    },
                );
                at += step;
            }
            out.push(start + over_ms, Act::Move { x: to_x, y: to_y });
            out.push(
                start + over_ms,
                Act::Up {
                    x: to_x,
                    y: to_y,
                    button,
                },
            );
        }
        out
    }

    /// The distinct event times, ascending.
    #[must_use]
    pub fn times(&self) -> Vec<u64> {
        let mut times = self
            .events
            .iter()
            .map(|event| event.at_ms)
            .collect::<Vec<_>>();
        times.dedup();
        times
    }
}

impl fmt::Display for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, event) in self.events.iter().enumerate() {
            if index > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{} @{}", event.act, event.at_ms)?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Script {
    type Err = ScriptError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Self::parse(source)
    }
}

#[cfg(test)]
mod tests {
    use super::{Act, Button, Mods, Script};

    #[test]
    fn times_are_absolute_relative_or_inherited() {
        let script = Script::parse("move 1,2 @100; click 3,4; key cmd-k +50; type \"a;b\" @400")
            .expect("parses");
        let times = script
            .events
            .iter()
            .map(|event| event.at_ms)
            .collect::<Vec<_>>();
        assert_eq!(times, [100, 100, 150, 400]);
        assert_eq!(
            script.events[1].act,
            Act::Click {
                x: 3.0,
                y: 4.0,
                button: Button::Left
            }
        );
        assert_eq!(
            script.events[3].act,
            Act::Type {
                text: "a;b".to_owned()
            }
        );
    }

    #[test]
    fn the_canonical_form_reads_back_unchanged() {
        let source = "hover 10.5,20 @0\ndown right 1,1 +16 # a comment\nscroll 5,5 0,-120\nhold cmd+alt @300\nrelease all @310\nresize 760x900\ntext-scale 150%\ndensity dense\ntheme glacier\ncontrast high\nmotion off\nleave\nkeys tab shift-tab @500\ntype \"say \\\"hi\\\"\"";
        let script = Script::parse(source).expect("parses");
        let canonical = script.to_string();
        assert_eq!(Script::parse(&canonical).expect("reparses"), script);
        assert!(canonical.contains("down right 1,1 @16"), "{canonical}");
        assert!(canonical.contains("hold cmd+alt @300"), "{canonical}");
        assert!(canonical.contains("release all @310"), "{canonical}");
        assert_eq!(script.events.len(), 15);
    }

    #[test]
    fn bad_statements_name_themselves() {
        let error = Script::parse("move 1,2; click 3 @40").expect_err("rejects");
        assert_eq!(error.statement, 2);
        assert!(error.message.contains("X,Y"), "{error}");
        let error = Script::parse("move 1,2 @50; move 1,2 @40").expect_err("rejects");
        assert!(error.message.contains("before"), "{error}");
        assert!(Script::parse("key @40").is_err());
        assert!(Script::parse("hold meta").is_err());
        assert!(Script::parse("density roomy").is_err());
        assert!(Script::parse("type \"open").is_err());
    }

    #[test]
    fn drags_expand_at_the_frame_period_and_zooms_and_routes_round_trip() {
        let script = Script::parse(
            "drag 100,100 -> 200,140 over 64 @10; wheel-zoom 300,200 1.25 @20; route symbol present::glyph::RelationLabel view=graph at=0.3.0 @30",
        )
        .expect("parses");
        assert_eq!(Script::parse(&script.to_string()).expect("reparses"), script);
        assert_eq!(script.end_ms(), 74);
        assert_eq!(
            script.events[2].act,
            Act::Route {
                target: "symbol present::glyph::RelationLabel view=graph at=0.3.0".to_owned()
            }
        );
        let expanded = script.expanded(16).to_string();
        assert_eq!(
            expanded,
            "down left 100,100 @10\nwheel-zoom 300,200 1.25 @20\nmove 125,110 @26\n\
             route symbol present::glyph::RelationLabel view=graph at=0.3.0 @30\n\
             move 150,120 @42\nmove 175,130 @58\nmove 200,140 @74\nup left 200,140 @74"
        );
        assert!(Script::parse("drag 1,1 -> 2,2").is_err());
        assert!(Script::parse("wheel-zoom 1,1 0").is_err());
    }

    #[test]
    fn modifier_sets_add_and_remove() {
        let held = Mods {
            cmd: true,
            ..Mods::default()
        }
        .with(Mods {
            alt: true,
            ..Mods::default()
        });
        assert_eq!(held.to_string(), "cmd+alt");
        assert_eq!(held.without(Mods::ALL), Mods::default());
        assert!(held.without(Mods::ALL).is_empty());
    }

    #[test]
    fn pushed_events_keep_time_order_and_instant_order() {
        let mut script = Script::parse("move 1,1 @10; move 2,2 @30").expect("parses");
        script.push(30, Act::Leave);
        script.push(20, Act::Leave);
        let rendered = script.to_string();
        assert_eq!(
            rendered,
            "move 1,1 @10\nleave @20\nmove 2,2 @30\nleave @30"
        );
    }
}
