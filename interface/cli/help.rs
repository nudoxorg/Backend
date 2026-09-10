//! Defines help behavior for `interface-cli`, whose purpose is to project the one shared local library onto a command line.
//! This module owns the help invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Everything the CLI says when it is not answering a command: help, the bare-`nudox` dashboard, and usage errors.
//!
//! The command list is [`COMMANDS`] verbatim. A capability that has a row in the registry and no
//! line in this help is a capability a person cannot find, which is the same as not shipping it.

use interface_library::{
    COMMANDS, Health, Shelf,
    render::{
        common::{Affordance, Affordances, Fault, write_fault},
        text::{Palette, TextOptions, Width, counts_line},
    },
};

use crate::args::{UsageError, UsageKind};

/// Every global option, spelled once so help and the parser cannot disagree.
const GLOBALS: [(&str, &str); 5] = [
    ("--format human|markdown|json", "How the reply is written; defaults to human."),
    ("--no-color", "Never emit an escape code, whatever the terminal says."),
    ("--root <path>", "Workspace root, overriding NUDOX_DATA_ROOT."),
    ("--detached", "Open the library read-only; adds are refused rather than queued."),
    ("-h, --help", "Print this help."),
];

const SEARCH_FLAGS: [(&str, &str); 5] = [
    ("--in <packages>", "Restrict to these packages; `cargo:serde` resolves against the shelf."),
    ("--kinds <kinds>", "Admit only these declaration kinds: fn, struct, trait, enum, …"),
    ("--lanes <lanes>", "Consult only these lanes: exact, names, graph, semantic."),
    ("--limit <n>", "Rows per page."),
    ("--cursor <n>", "Continue a truncated page."),
];

const GRAPH_FLAGS: [(&str, &str); 4] = [
    ("--kind <relation>", "One relation: calls, implements, references, imports, …"),
    ("--incoming | --outgoing", "Traversal direction; outgoing by default."),
    ("--depth <n>", "Hops, up to four."),
    ("--limit <n>", "Edge budget."),
];

/// The full help: usage, every registry row with its shared description, and every option.
#[must_use]
pub(crate) fn help_text(palette: Palette, width: Width) -> String {
    let mut out = String::new();
    line(&mut out, &palette.strong("nudox"));
    line(&mut out, &palette.dim("read compiled packages: their pages, relations, and text"));
    out.push('\n');
    line(&mut out, "usage: nudox [options] <command> [operand] [flags]");
    out.push('\n');
    line(&mut out, &palette.dim("commands"));
    let column = COMMANDS.into_iter().map(|row| row.name.len()).max().unwrap_or(0) + 2;
    for row in COMMANDS {
        rows(&mut out, row.name, column, row.description, palette, width);
    }
    section(&mut out, "options", &GLOBALS, palette, width);
    section(&mut out, "search flags", &SEARCH_FLAGS, palette, width);
    section(&mut out, "graph flags", &GRAPH_FLAGS, palette, width);
    out
}

fn section(
    out: &mut String,
    title: &str,
    entries: &[(&str, &str)],
    palette: Palette,
    width: Width,
) {
    out.push('\n');
    line(out, &palette.dim(title));
    let column = entries.iter().map(|(name, _)| name.len()).max().unwrap_or(0) + 2;
    for (name, description) in entries {
        rows(out, name, column, description, palette, width);
    }
}

fn rows(
    out: &mut String,
    name: &str,
    column: usize,
    description: &str,
    palette: Palette,
    width: Width,
) {
    let pad = " ".repeat(column.saturating_sub(name.chars().count()));
    if 2 + column + description.chars().count() <= width.columns() {
        line(out, &format!("  {name}{pad}{}", palette.dim(description)));
    } else {
        line(out, &format!("  {name}"));
        line(out, &format!("    {}", palette.dim(description)));
    }
}

/// What a bare `nudox` prints: where the library stands, and three commands worth typing.
#[must_use]
pub(crate) fn dashboard_text(
    shelf: Option<&Shelf>,
    health: &Health,
    options: &TextOptions<'_>,
) -> String {
    let palette = options.palette;
    let mut out = String::new();
    line(&mut out, &shelf_summary(shelf, palette));
    line(&mut out, &palette.dim(&health_summary(health)));
    out.push('\n');
    line(&mut out, &palette.dim("try"));
    for example in [
        "nudox packages",
        "nudox search \"deserialize map\"",
        "nudox show cargo:serde@1.0.196::de::Deserializer",
    ] {
        line(&mut out, &format!("  {example}"));
    }
    out
}

fn shelf_summary(shelf: Option<&Shelf>, palette: Palette) -> String {
    let Some(shelf) = shelf else {
        return palette.dim("the shelf could not be read");
    };
    if shelf.entries.is_empty() {
        return palette.dim("no packages yet");
    }
    let total = shelf.entries.len();
    let noun = if total == 1 { "package" } else { "packages" };
    let mut census = interface_documents::Census::default();
    for card in shelf.ready() {
        census.entities = interface_documents::Count(
            census.entities.0.saturating_add(card.census.entities.0),
        );
        census.public =
            interface_documents::Count(census.public.0.saturating_add(card.census.public.0));
        census.documented = interface_documents::Count(
            census.documented.0.saturating_add(card.census.documented.0),
        );
    }
    format!(
        "{total} {noun} \u{b7} {} ready \u{b7} {}",
        shelf.ready().count(),
        counts_line(&census)
    )
}

fn health_summary(health: &Health) -> String {
    health
        .rows()
        .iter()
        .map(|(capability, state)| {
            format!(
                "{} {}",
                interface_library::render::common::capability_glyph(state),
                capability.label()
            )
        })
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// How the command line itself spells the one call that could make progress.
///
/// A usage error is the one failure whose next step is almost always the help, so
/// [`Affordance::None`] is answered with it rather than with a shrug.
struct UsageAffordances {
    command: Option<&'static str>,
}

impl Affordances for UsageAffordances {
    fn spell(&self, affordance: &Affordance) -> Option<String> {
        match affordance {
            Affordance::None => Some(match self.command {
                Some(command) => format!("nudox help  (for `{command}`)"),
                None => "nudox help".to_owned(),
            }),
            Affordance::Packages => Some("nudox packages".to_owned()),
            Affordance::Health => Some("nudox health".to_owned()),
            Affordance::Add { package } => Some(format!("nudox add {package}")),
            Affordance::Resolve { text } => Some(format!("nudox resolve \"{text}\"")),
            Affordance::Search { query } => Some(format!("nudox search \"{query}\"")),
            Affordance::Show { address } => Some(format!("nudox show {address}")),
        }
    }
}

/// Renders one usage error in the same three-line fault grammar every other failure uses.
#[must_use]
pub(crate) fn usage_text(error: &UsageError, palette: Palette) -> String {
    let fault = Fault::new(error.kind.slug(), error.token.clone(), Affordance::None)
        .detailed(usage_detail(error));
    let mut out = String::new();
    write_fault(&mut out, &fault, &UsageAffordances { command: error.command });
    for row in usage_evidence(error) {
        line(&mut out, &palette.dim(&format!("  {row}")));
    }
    out
}

/// The sentence that names what the parser observed, in the failure's own terms.
#[must_use]
pub(crate) fn usage_detail(error: &UsageError) -> String {
    let scope = error.command.unwrap_or("nudox");
    match &error.kind {
        UsageKind::UnknownCommand { .. } => "not one of the nine commands".to_owned(),
        UsageKind::UnknownFlag => format!("`{scope}` does not accept this flag"),
        UsageKind::MissingValue => "this flag needs a value".to_owned(),
        UsageKind::MissingOperand { operand } => format!("`{scope}` needs {operand}"),
        UsageKind::UnexpectedOperand => format!("`{scope}` takes one operand"),
        UsageKind::BadValue { .. } => "not a value this flag accepts".to_owned(),
        UsageKind::BadOperand { cause } => cause.clone(),
        UsageKind::ConflictingFlags { other } => format!("cannot be given together with {other}"),
    }
}

/// Extra lines that turn a rejection into a next attempt.
#[must_use]
pub(crate) fn usage_evidence(error: &UsageError) -> Vec<String> {
    match &error.kind {
        UsageKind::UnknownCommand { nearest } if !nearest.is_empty() => {
            vec![format!("did you mean: {}", nearest.join(", "))]
        }
        UsageKind::UnknownCommand { .. } => {
            vec![format!(
                "commands: {}",
                COMMANDS.into_iter().map(|row| row.name).collect::<Vec<_>>().join(", ")
            )]
        }
        UsageKind::BadValue { accepted } if !accepted.is_empty() => {
            vec![format!("accepted: {}", accepted.join(", "))]
        }
        _ => Vec::new(),
    }
}

fn line(out: &mut String, text: &str) {
    out.push_str(text);
    out.push('\n');
}
