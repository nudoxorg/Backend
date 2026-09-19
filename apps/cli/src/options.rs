//! Global CLI options, and the theme they produce.
//!
//! Three things decide how output looks, and all three are honoured here so no
//! renderer has to guess: the requested `--format`, whether colour is allowed
//! (`--no-color`, `NO_COLOR`, or stdout not being a terminal), and how wide the
//! reader's terminal is (`COLUMNS`). A redirected run and a `--no-color` run
//! produce byte-identical output, which is what makes the captured goldens in
//! the journeys meaningful.

use backend_present::{Fault, Theme, Width};
use std::io::IsTerminal as _;
use std::path::PathBuf;

/// Which rendering the caller asked for.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Format {
    /// Painted, width-aware terminal output.
    #[default]
    Human,
    /// The same content as the MCP text block, byte for byte.
    Markdown,
    /// The stable typed JSON projection of the presentation model.
    Json,
}

impl Format {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "human" | "text" => Some(Self::Human),
            "markdown" | "md" => Some(Self::Markdown),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    /// Returns the stable lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }
}

/// Everything the surface needs before it knows which command was asked for.
#[derive(Clone, Debug)]
pub struct Options {
    format: Format,
    theme: Theme,
    project: Option<PathBuf>,
    workspace: Option<PathBuf>,
    endpoint: Option<PathBuf>,
    limit: Option<String>,
}

impl Options {
    /// The options a failure before option parsing is reported with.
    ///
    /// A caller whose `--format` was itself unreadable still deserves a
    /// readable failure, so this is a plain human theme with no colour.
    #[must_use]
    pub fn fallback() -> Self {
        Self {
            format: Format::Human,
            theme: Theme::plain(),
            project: None,
            workspace: None,
            endpoint: None,
            limit: None,
        }
    }

    /// The options a non-interactive caller renders one format with.
    ///
    /// This is `fallback` with a chosen rendering: no colour, default width,
    /// no endpoint. It exists so a test can ask for the exact bytes a piped
    /// run produces without composing an argument vector to get them.
    #[must_use]
    pub fn plain(format: Format) -> Self {
        Self {
            format,
            ..Self::fallback()
        }
    }

    /// Returns the requested rendering.
    #[must_use]
    pub const fn format(&self) -> Format {
        self.format
    }

    /// Returns the theme every renderer paints with.
    #[must_use]
    pub const fn theme(&self) -> Theme {
        self.theme
    }

    /// Returns the selected project directory.
    #[must_use]
    pub const fn project(&self) -> Option<&PathBuf> {
        self.project.as_ref()
    }

    /// Returns the selected durable workspace.
    #[must_use]
    pub const fn workspace(&self) -> Option<&PathBuf> {
        self.workspace.as_ref()
    }

    /// Returns the selected local endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> Option<&PathBuf> {
        self.endpoint.as_ref()
    }

    /// Returns the global page bound, when the caller supplied one.
    #[must_use]
    pub fn limit(&self) -> Option<&str> {
        self.limit.as_deref()
    }

    /// Returns whether the caller asked for a machine rendering.
    #[must_use]
    pub const fn is_machine(&self) -> bool {
        matches!(self.format, Format::Json)
    }
}

/// Splits global options from the command words that follow them.
///
/// # Errors
///
/// Returns a usage fault naming the exact option that was wrong.
pub fn split(args: &[String]) -> Result<(Options, Vec<String>), Fault> {
    let mut format = None;
    let mut colour = None;
    let mut project = None;
    let mut workspace = None;
    let mut endpoint = None;
    let mut limit = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut at = 0_usize;
    while let Some(argument) = args.get(at) {
        at = at.saturating_add(1);
        match argument.as_str() {
            "--json" => format = Some(Format::Json),
            "--no-color" | "--no-colour" => colour = Some(false),
            "--color" | "--colour" => colour = Some(true),
            "--format" => format = Some(take_format(args, &mut at)?),
            "--project" => project = Some(PathBuf::from(take(args, &mut at, "--project")?)),
            "--workspace" => workspace = Some(PathBuf::from(take(args, &mut at, "--workspace")?)),
            "--endpoint" => endpoint = Some(PathBuf::from(take(args, &mut at, "--endpoint")?)),
            "--limit" => limit = Some(take(args, &mut at, "--limit")?),
            other => rest.push(other.to_owned()),
        }
    }
    let format = format.unwrap_or_default();
    Ok((
        Options {
            format,
            theme: theme_for(format, colour),
            project,
            workspace,
            endpoint,
            limit,
        },
        rest,
    ))
}

fn take(args: &[String], at: &mut usize, option: &str) -> Result<String, Fault> {
    let value = args
        .get(*at)
        .ok_or_else(|| Fault::usage(option, format!("{option} needs a value after it")))?;
    *at = at.saturating_add(1);
    Ok(value.clone())
}

fn take_format(args: &[String], at: &mut usize) -> Result<Format, Fault> {
    let value = take(args, at, "--format")?;
    Format::parse(&value).ok_or_else(|| {
        Fault::usage(
            "--format",
            format!("`{value}` is not a rendering; choose human, markdown, or json"),
        )
    })
}

/// Builds the theme from the format, the caller's choice, and the environment.
fn theme_for(format: Format, requested: Option<bool>) -> Theme {
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|columns| columns.parse::<usize>().ok())
        .map_or_else(Width::default, Width::new);
    let allowed = match requested {
        Some(choice) => choice,
        None => {
            std::env::var_os("NO_COLOR").is_none()
                && format == Format::Human
                && std::io::stdout().is_terminal()
        }
    };
    if allowed && format == Format::Human {
        Theme::coloured(width)
    } else {
        Theme::plain().with_width(width)
    }
}

/// Returns whether the reader's diagnostics stream is a terminal.
///
/// Progress rewrites its own line, which is noise in a log and motion in a
/// terminal, so it is emitted only when someone is watching.
#[must_use]
pub fn stderr_is_terminal() -> bool {
    std::io::stderr().is_terminal()
}
