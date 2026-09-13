//! The two renderings of the presentation model, and the theme they share.
//!
//! There are exactly two: [`text`] for a terminal and [`markdown`] for an
//! agent. The desktop renders the model directly into its own widgets and does
//! not need a third string form. Both renderings read the *same* values in the
//! *same* order, so CLI `--format markdown` and an MCP `tools/call` text block
//! are byte-identical for the same reply — which is what makes the parity
//! journeys assertable on content rather than on shape.
//!
//! Colour lives here and nowhere else. A [`Theme`] carries whether colour is
//! allowed and how wide the reader's terminal is; a renderer asks the theme to
//! paint a [`Style`] and gets either an escape-wrapped string or the plain
//! text. `NO_COLOR` and a missing TTY both produce the same plain output, so a
//! redirected CLI run and a Markdown block cannot disagree.

pub mod markdown;
pub mod text;

use core::fmt;

/// The role one painted span plays, independent of any colour choice.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Style {
    /// A section or record heading.
    Heading,
    /// The project part of an identity trail.
    Project,
    /// The declaration's own name.
    Name,
    /// The exact coordinate an agent passes back.
    Coordinate,
    /// Secondary metadata: kinds, tags, counts, lane names.
    Dim,
    /// A reserved word inside a signature.
    Keyword,
    /// An identifier in type position inside a signature.
    Type,
    /// A parameter or field name inside a signature.
    Binding,
    /// A lifetime inside a signature.
    Lifetime,
    /// A literal inside a signature.
    Literal,
    /// Structural punctuation inside a signature.
    Punctuation,
    /// A failure, or the reason a lane answered nothing.
    Fault,
    /// A lane or project that covered its scope.
    Ready,
    /// A lane or project that is still working.
    Working,
}

impl Style {
    /// Returns the SGR parameters this style paints with.
    const fn sgr(self) -> &'static str {
        match self {
            Self::Heading | Self::Name => "1",
            Self::Project => "1;34",
            Self::Coordinate | Self::Dim | Self::Punctuation => "2",
            Self::Keyword => "35",
            Self::Type => "36",
            Self::Binding => "0",
            Self::Lifetime | Self::Working => "33",
            Self::Literal | Self::Ready => "32",
            Self::Fault => "31",
        }
    }
}

/// How wide the reader's rendering surface is.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Width(usize);

impl Width {
    /// Admits a terminal width, clamped to a range the layout still reads at.
    #[must_use]
    pub const fn new(columns: usize) -> Self {
        if columns < crate::MINIMUM_WIDTH {
            Self(crate::MINIMUM_WIDTH)
        } else if columns > 200 {
            Self(200)
        } else {
            Self(columns)
        }
    }

    /// Returns the usable column count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl Default for Width {
    fn default() -> Self {
        Self::new(crate::DEFAULT_WIDTH)
    }
}

/// Whether colour is allowed, and how wide the reader's surface is.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Theme {
    colour: Colour,
    width: Width,
}

/// Whether a rendering may emit terminal escapes.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Colour {
    /// Emit SGR escapes.
    Enabled,
    /// Emit plain text only.
    #[default]
    Disabled,
}

impl Theme {
    /// A plain theme at the default width: what a pipe, a file, and Markdown get.
    #[must_use]
    pub fn plain() -> Self {
        Self::default()
    }

    /// A coloured theme at one width.
    #[must_use]
    pub const fn coloured(width: Width) -> Self {
        Self {
            colour: Colour::Enabled,
            width,
        }
    }

    /// Returns this theme at a different width.
    #[must_use]
    pub const fn with_width(mut self, width: Width) -> Self {
        self.width = width;
        self
    }

    /// Returns this theme with colour forced off.
    #[must_use]
    pub const fn without_colour(mut self) -> Self {
        self.colour = Colour::Disabled;
        self
    }

    /// Returns the usable width.
    #[must_use]
    pub const fn width(self) -> Width {
        self.width
    }

    /// Returns whether this theme paints.
    #[must_use]
    pub const fn is_coloured(self) -> bool {
        matches!(self.colour, Colour::Enabled)
    }

    /// Paints one span, or returns it unchanged when colour is off.
    #[must_use]
    pub fn paint(self, style: Style, text: &str) -> String {
        if !self.is_coloured() || text.is_empty() {
            return text.to_owned();
        }
        format!("\u{1b}[{}m{text}\u{1b}[0m", style.sgr())
    }

    /// Truncates `text` to fit `budget` display columns, with an ellipsis.
    ///
    /// Coordinates are never passed through this: they are the one thing a
    /// reader must be able to copy whole.
    #[must_use]
    pub fn clip(self, text: &str, budget: usize) -> String {
        if display_width(text) <= budget || budget == 0 {
            return text.to_owned();
        }
        let mut kept = String::with_capacity(budget);
        let mut used = 0_usize;
        for character in text.chars() {
            let next = used.saturating_add(1);
            if next >= budget {
                break;
            }
            kept.push(character);
            used = next;
        }
        kept.push('…');
        kept
    }
}

/// Returns the display column count of one line, counting each `char` once.
#[must_use]
pub fn display_width(text: &str) -> usize {
    text.chars().filter(|character| *character != '\u{1b}').count()
}

/// A bounded writer that never grows past one frame.
pub(crate) struct Lines {
    buffer: String,
}

impl Lines {
    pub(crate) fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    pub(crate) fn push(&mut self, line: impl AsRef<str>) {
        self.buffer.push_str(line.as_ref());
        self.buffer.push('\n');
    }

    pub(crate) fn blank(&mut self) {
        if !self.buffer.is_empty() && !self.buffer.ends_with("\n\n") {
            self.buffer.push('\n');
        }
    }

    pub(crate) fn finish(self) -> String {
        self.buffer
    }
}

impl fmt::Display for Lines {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.buffer)
    }
}
