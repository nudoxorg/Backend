//! The CLI's argument grammar, generated from the shared command registry.
//!
//! There is no hand-written parser per verb. [`backend_present::GRAMMARS`]
//! says what every registry row takes; this module walks one argument vector
//! against the row the caller named and produces an [`Invocation`]. Help is
//! generated the same way, grouped by [`backend_library::CommandDomain`], so a
//! command can never exist without appearing in `--help` and a help line can
//! never describe a grammar the parser does not accept.

use backend_library::{CommandDomain, COMMANDS};
use backend_present::{
    ArgumentKind, ArgumentSpec, CommandGrammar, Fault, GRAMMARS, Invocation, domain_name, domains,
    grammar_for, grammars_in,
};
use core::fmt::Write as _;

/// The escape hatch that takes one tagged `SurfaceCommand` JSON object.
pub const SURFACE_VERB: &str = "surface";

/// Parses one argument vector against the shared command registry.
///
/// # Errors
///
/// Returns a usage fault naming the unknown command, the unknown option, or
/// the operand count the grammar expects.
pub fn parse(args: &[String], default_limit: Option<&str>) -> Result<Invocation, Fault> {
    let Some(spelling) = args.first().map(String::as_str) else {
        return Err(Fault::usage(
            "command",
            "name a command; run `backend --help` for the whole vocabulary",
        ));
    };
    let grammar = grammar_for(spelling).ok_or_else(|| unknown_command(spelling))?;
    let mut invocation = Invocation::new(grammar);
    let mut at = 1_usize;
    while let Some(argument) = args.get(at) {
        at = at.saturating_add(1);
        let Some(name) = argument.strip_prefix("--") else {
            invocation.push(argument.clone());
            continue;
        };
        let spec = option_named(grammar, name).ok_or_else(|| unknown_option(grammar, name))?;
        if spec.kind() == ArgumentKind::Flag {
            invocation.raise(spec.name());
            continue;
        }
        let value = args.get(at).ok_or_else(|| {
            Fault::usage(spec.name(), format!("--{} needs a value: {}", spec.name(), spec.help()))
        })?;
        at = at.saturating_add(1);
        invocation.set(spec.name(), value.clone());
    }
    if let Some(limit) = default_limit
        && option_named(grammar, "limit").is_some()
        && invocation.option("limit").is_none()
    {
        invocation.set("limit", limit);
    }
    invocation.check()?;
    Ok(invocation)
}

fn option_named(grammar: CommandGrammar, name: &str) -> Option<ArgumentSpec> {
    grammar
        .options()
        .iter()
        .copied()
        .find(|option| option.name() == name)
}

fn unknown_command(spelling: &str) -> Fault {
    let nearest = nearest_command(spelling);
    Fault::usage(
        spelling,
        nearest.map_or_else(
            || format!("`{spelling}` is not a command; run `backend --help` for the whole vocabulary"),
            |nearest| format!("`{spelling}` is not a command; did you mean `{nearest}`?"),
        ),
    )
}

fn unknown_option(grammar: CommandGrammar, name: &str) -> Fault {
    let known = grammar
        .options()
        .iter()
        .map(|option| format!("--{}", option.name()))
        .collect::<Vec<_>>();
    Fault::usage(
        format!("--{name}"),
        if known.is_empty() {
            format!("{} takes no options", grammar.name())
        } else {
            format!("{} takes only {}", grammar.name(), known.join(", "))
        },
    )
}

/// Returns the nearest registry spelling within a small edit distance.
///
/// A mistyped verb is the most common way to meet this surface, and "did you
/// mean" is worth more to a reader than the whole vocabulary reprinted.
fn nearest_command(spelling: &str) -> Option<&'static str> {
    const NEAR: usize = 3;
    GRAMMARS
        .iter()
        .flat_map(|grammar| {
            core::iter::once(grammar.name()).chain(grammar.aliases().iter().copied())
        })
        .map(|name| (distance(name, spelling), name))
        .filter(|(distance, _)| *distance <= NEAR)
        .min()
        .map(|(_, name)| name)
}

/// Returns the Levenshtein distance between two spellings.
fn distance(left: &str, right: &str) -> usize {
    let right_characters: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right_characters.len()).collect();
    for (row, from) in left.chars().enumerate() {
        let mut current = Vec::with_capacity(previous.len());
        current.push(row.saturating_add(1));
        for (column, to) in right_characters.iter().enumerate() {
            let at = |row: &[usize], index: usize| row.get(index).copied().unwrap_or(usize::MAX);
            let substitute = at(&previous, column).saturating_add(usize::from(from != *to));
            let insert = at(&previous, column.saturating_add(1)).saturating_add(1);
            let delete = at(&current, column).saturating_add(1);
            current.push(substitute.min(insert).min(delete));
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(usize::MAX)
}

/// Renders the whole command vocabulary, grouped by domain.
#[must_use]
pub fn help() -> String {
    let mut out = String::from(
        "backend — local-first, versioned code intelligence\n\n\
         Usage: backend [OPTIONS] <COMMAND> [OPERANDS]\n",
    );
    for domain in domains() {
        let _ = writeln!(out, "\n{}", domain_name(domain));
        for grammar in grammars_in(domain) {
            out.push_str(&help_line(grammar));
        }
    }
    let _ = writeln!(
        out,
        "\nescape hatch\n  {:<38}  Execute any tagged SurfaceCommand object verbatim.",
        "surface <JSON>"
    );
    out.push_str(OPTIONS);
    out
}

fn help_line(grammar: CommandGrammar) -> String {
    let description = grammar
        .spec()
        .map_or("", |spec| spec.description)
        .split('.')
        .next()
        .unwrap_or_default();
    format!("  {:<38}  {description}.\n", grammar.usage())
}

/// Renders the long help for one command, including when to reach for it.
#[must_use]
pub fn help_for(grammar: CommandGrammar) -> String {
    let mut out = format!("backend {}\n\n", grammar.usage());
    let _ = writeln!(out, "{}", grammar.description());
    if !grammar.positional().is_empty() {
        out.push_str("\noperands\n");
        for spec in grammar.positional() {
            let _ = writeln!(out, "  {:<14}  {}", spec.kind().metavar(), spec.help());
        }
    }
    if !grammar.options().is_empty() {
        out.push_str("\noptions\n");
        for spec in grammar.options() {
            let _ = writeln!(out, "  --{:<12}  {}", spec.name(), spec.help());
        }
    }
    if !grammar.aliases().is_empty() {
        let _ = writeln!(out, "\nalso spelled: {}", grammar.aliases().join(", "));
    }
    out
}

/// Returns whether the closed registry still has the size this surface covers.
#[must_use]
pub fn registry_is_covered() -> bool {
    COMMANDS.len() == GRAMMARS.len()
        && COMMANDS
            .iter()
            .all(|spec| grammar_for(spec.name).is_some_and(|row| row.name() == spec.name))
}

/// Returns the domains in the order help prints them.
#[must_use]
pub fn help_domains() -> [CommandDomain; 5] {
    domains()
}

const OPTIONS: &str = "
options
  --format human|markdown|json  Choose the rendering; markdown matches the MCP text block.
  --json                        Shorthand for --format json.
  --no-color                    Never paint; NO_COLOR and a non-terminal stdout do the same.
  --limit COUNT                 Bound any command that pages.
  --project PATH                Select the project; defaults to the current directory.
  --workspace PATH              Select durable state; defaults to <project>/.backend/v2.
  --endpoint PATH               Connect to a specific local daemon.
  -h, --help                    Show this help, or one command's help after its name.
  -V, --version                 Show the version.

COLUMNS is honoured for width. Exit codes: 0 success, 2 the engine refused,
64 the arguments were wrong, 1 the local endpoint failed.
";
