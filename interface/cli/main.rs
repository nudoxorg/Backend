//! Runs the `interface-cli` executable, which exists to project the one shared local library onto a command line.
//! Process setup is kept here while product policy remains in library crates.
//! Every external failure crosses this boundary as a structured diagnostic.
//! The `nudox` process: argv to one [`Command`], one [`Reply`] to text, Markdown, or JSON, and one exit code.
//!
//! Four exit codes and nothing else. `0` is a reply that carried no failure. `2` is a typed
//! business failure — an unknown package, an absent shelf row, a search where every requested lane
//! refused to run — which is a real answer, not a crash. `64` is a command line this process could
//! not read. `1` is the process failing to do its own job: the workspace root, the library, or
//! standard output.

use std::{
    io::{self, IsTerminal as _, Write as _},
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use interface_documents::ProjectionLimits;
use interface_identity::PackageCoordinate;
use interface_library::{
    AddOutcome, AddProgress, Command, CompilerAttachment, Library, LibraryOpenError, OpenOptions,
    RemoveOutcome, Reply, Resolution, Shelf, Timestamp, WorkspaceRoot, WorkspaceRootError,
    render::{
        common::RenderContext,
        text::{self, Attachment, Palette, TextOptions, Width, every_requested_lane_unavailable},
    },
};
use interface_search::{GraphRequest, SearchRequest, SearchScope};

mod args;
mod help;
mod json;
mod markdown;
mod progress;

use args::{Attach, ColourChoice, Format, Globals, Plan, SearchPlan, UsageError, UsageKind};
use progress::{AddProgressWriter, Liveness};

/// A command line this process could not read.
const USAGE_EXIT: u8 = 64;
/// A reply that carried a typed failure.
const BUSINESS_EXIT: u8 = 2;
/// This process failed at its own job.
const PROCESS_EXIT: u8 = 1;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (format, colour) = early_globals(&arguments);
    match args::parse(arguments) {
        Ok(invocation) => dispatch(invocation),
        Err(error) => report_usage(&error, format, palette_for(format, colour)),
    }
}

/// Reads just enough of argv to know how to render a rejection of argv.
///
/// The parser is the thing that failed, so it cannot be asked how the failure should be spelled.
fn early_globals(arguments: &[String]) -> (Format, ColourChoice) {
    let mut format = Format::Human;
    let mut colour = ColourChoice::Auto;
    let mut expecting = false;
    for argument in arguments {
        let value = if expecting {
            expecting = false;
            Some(argument.as_str())
        } else {
            argument.strip_prefix("--format=")
        };
        match value {
            Some("json") => format = Format::Json,
            Some("markdown") => format = Format::Markdown,
            Some(_) | None => {}
        }
        if argument == "--format" {
            expecting = true;
        }
        if argument == "--no-color" {
            colour = ColourChoice::Never;
        }
    }
    (format, colour)
}

fn dispatch(invocation: args::Invocation) -> ExitCode {
    let globals = invocation.globals;
    let palette = palette_for(globals.format, globals.colour);
    if matches!(invocation.plan, Plan::Help) {
        return emit(&help::help_text(palette, terminal_width()));
    }
    let library = match open_library(&globals) {
        Ok(library) => library,
        Err(text) => return fail(&text),
    };
    let shelf = library.shelf().ok();
    let neighbours = coordinates(shelf.as_ref());
    let options = TextOptions {
        width: terminal_width(),
        palette,
        context: RenderContext { now: now() },
        attachment: match globals.attach {
            Attach::Production => Attachment::Attached,
            Attach::Detached => Attachment::Detached,
        },
        neighbours: &neighbours,
        subject: invocation.plan.subject(),
    };
    if matches!(invocation.plan, Plan::Dashboard) {
        return emit(&help::dashboard_text(shelf.as_ref(), &library.health(), &options));
    }
    answer(&library, invocation.plan, &globals, &options)
}

fn answer(
    library: &Library,
    plan: Plan,
    globals: &Globals,
    options: &TextOptions<'_>,
) -> ExitCode {
    let name = plan.command_name();
    let interactive = plan_is_add(&plan) && globals.format == Format::Human;
    let command = match command_of(plan, options.neighbours) {
        Ok(command) => command,
        Err(error) => return report_usage(&error, globals.format, options.palette),
    };
    let mut writer = progress_writer(&command, interactive, options.palette);
    let started = Instant::now();
    let reply = match writer.as_mut() {
        Some(writer) => library.execute(command, &mut |event| writer.observe(event)),
        None => library.execute(command, &mut |_| {}),
    };
    if let Some(writer) = writer.as_mut() {
        writer.clear();
    }
    let failed = carries_failure(&reply);
    let rendered = match present(globals.format, name, &reply, !failed, options) {
        Ok(rendered) => rendered,
        Err(text) => return fail(&text),
    };
    let rendered = elapsed_suffix(rendered, &reply, globals.format, started.elapsed(), options);
    let written = emit(&rendered);
    if failed && written == ExitCode::SUCCESS {
        ExitCode::from(BUSINESS_EXIT)
    } else {
        written
    }
}

const fn plan_is_add(plan: &Plan) -> bool {
    matches!(plan, Plan::Add { .. })
}

fn progress_writer(
    command: &Command,
    interactive: bool,
    palette: Palette,
) -> Option<AddProgressWriter> {
    let Command::Add { url } = command else {
        return None;
    };
    if !interactive {
        return None;
    }
    let coordinate = PackageCoordinate::from_package_url(url)
        .map_or_else(|| url.as_ref().to_owned(), |coordinate| coordinate.to_string());
    let liveness = if io::stderr().is_terminal() {
        Liveness::Interactive
    } else {
        Liveness::Logged
    };
    Some(AddProgressWriter::new(coordinate, liveness, palette))
}

/// Turns one plan into the one dispatch, resolving `--in` against the shelf on the way.
fn command_of(plan: Plan, shelf: &[PackageCoordinate]) -> Result<Command, UsageError> {
    Ok(match plan {
        Plan::Help | Plan::Dashboard | Plan::Packages => Command::Packages,
        Plan::Add { url, .. } => Command::Add { url },
        Plan::Remove { coordinate, .. } => Command::Remove { coordinate },
        Plan::Show { locator, .. } => Command::Show {
            locator,
            limits: ProjectionLimits::default(),
        },
        Plan::Outline { coordinate, .. } => Command::Outline { coordinate },
        Plan::Resolve { spelled } => Command::Resolve {
            text: spelled.into_boxed_str(),
        },
        Plan::Search(search) => Command::Search(search_request(search, shelf)?),
        Plan::Graph(graph) => Command::Graph(GraphRequest {
            source: graph.source,
            kind: graph.kind,
            direction: graph.direction,
            depth: graph.depth,
            limit: graph.limit,
        }),
        Plan::Health => Command::Health,
    })
}

fn search_request(
    plan: SearchPlan,
    shelf: &[PackageCoordinate],
) -> Result<SearchRequest, UsageError> {
    let mut packages = Vec::new();
    for spelled in &plan.scope {
        packages.push(resolve_scope_entry(spelled, shelf)?);
    }
    Ok(SearchRequest {
        text: plan.text,
        scope: SearchScope {
            packages: (!packages.is_empty()).then(|| packages.into_boxed_slice()),
            kinds: plan.kinds,
        },
        lanes: plan.lanes,
        limit: plan.limit,
        cursor: plan.cursor,
    })
}

/// Resolves one `--in` operand: a pinned coordinate is taken as written, and anything else is
/// matched against the shelf, because a person who has one version of `serde` loaded should not
/// have to type its version to search inside it.
fn resolve_scope_entry(
    spelled: &str,
    shelf: &[PackageCoordinate],
) -> Result<PackageCoordinate, UsageError> {
    if let Ok(coordinate) = PackageCoordinate::parse(spelled) {
        return Ok(coordinate);
    }
    let matches: Vec<&PackageCoordinate> =
        shelf.iter().filter(|entry| names_package(entry, spelled)).collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(scope_error(
            spelled,
            shelf.iter().map(ToString::to_string).collect(),
        )),
        several => Err(scope_error(
            spelled,
            several.iter().map(ToString::to_string).collect(),
        )),
    }
}

fn scope_error(spelled: &str, accepted: Vec<String>) -> UsageError {
    UsageError {
        kind: UsageKind::BadValue { accepted },
        token: spelled.to_owned(),
        command: Some("search"),
    }
}

fn names_package(entry: &PackageCoordinate, spelled: &str) -> bool {
    match spelled.split_once(':') {
        Some((ecosystem, name)) => {
            interface_identity::ecosystem_tag(entry.ecosystem).as_str() == ecosystem
                && entry.name.as_str() == name
        }
        None => entry.name.as_str() == spelled,
    }
}

fn present(
    format: Format,
    command: &'static str,
    reply: &Reply,
    ok: bool,
    options: &TextOptions<'_>,
) -> Result<String, String> {
    match format {
        Format::Human => Ok(text::render(reply, options)),
        Format::Markdown => Ok(markdown::render(reply, &options.context)),
        Format::Json => json::reply_json(command, reply, ok, options)
            .map(|line| format!("{line}\n"))
            .map_err(|error| format!("the reply could not be encoded as JSON: {error}")),
    }
}

/// A finished add says how long it took, because that is the fact the reader was waiting for.
fn elapsed_suffix(
    rendered: String,
    reply: &Reply,
    format: Format,
    elapsed: Duration,
    options: &TextOptions<'_>,
) -> String {
    if format != Format::Human || !matches!(reply, Reply::Added(AddOutcome::Ready { .. })) {
        return rendered;
    }
    let trimmed = rendered.trim_end_matches('\n');
    format!(
        "{trimmed}  {}\n",
        options
            .palette
            .dim(&format!("{:.1}s", elapsed.as_secs_f64()))
    )
}

/// Whether a reply carried a typed failure. `health` never does: reporting a detached compiler is
/// the command working, not the command failing.
fn carries_failure(reply: &Reply) -> bool {
    match reply {
        Reply::Health(_) => false,
        Reply::Packages(result) => result.is_err(),
        Reply::Added(outcome) => !matches!(outcome, AddOutcome::Ready { .. }),
        Reply::Removed(outcome) => !matches!(outcome, RemoveOutcome::Removed),
        Reply::Page(result) => result.is_err(),
        Reply::Outline(result) => result.is_err(),
        Reply::Graphed(result) => result.is_err(),
        Reply::Resolved(Err(_)) => true,
        Reply::Resolved(Ok(resolution)) => !matches!(resolution, Resolution::Exact(_)),
        Reply::Searched(terminal) => every_requested_lane_unavailable(terminal),
    }
}

fn report_usage(error: &UsageError, format: Format, palette: Palette) -> ExitCode {
    let rendered = match format {
        Format::Json => match json::usage_json(error) {
            Ok(line) => format!("{line}\n"),
            Err(cause) => return fail(&format!("the usage error could not be encoded: {cause}")),
        },
        Format::Human | Format::Markdown => help::usage_text(error, palette),
    };
    if emit(&rendered) == ExitCode::SUCCESS {
        ExitCode::from(USAGE_EXIT)
    } else {
        ExitCode::from(PROCESS_EXIT)
    }
}

fn open_library(globals: &Globals) -> Result<Library, String> {
    let root = workspace_root(globals.root.clone())?;
    let compiler = match globals.attach {
        Attach::Production => CompilerAttachment::Production,
        Attach::Detached => CompilerAttachment::Detached,
    };
    Library::open(OpenOptions { root, compiler }).map_err(open_detail)
}

fn workspace_root(explicit: Option<PathBuf>) -> Result<WorkspaceRoot, String> {
    let Some(path) = explicit else {
        return WorkspaceRoot::resolve().map_err(root_detail);
    };
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| format!("the working directory could not be read: {error}"))?
            .join(path)
    };
    WorkspaceRoot::absolute(absolute, "--root").map_err(root_detail)
}

fn root_detail(error: WorkspaceRootError) -> String {
    match error {
        WorkspaceRootError::Missing { variable } => {
            format!("no workspace root: {variable} is not set")
        }
        WorkspaceRootError::Relative { variable, path } => {
            format!("the root from {variable} is relative: {}", path.display())
        }
    }
}

fn open_detail(error: LibraryOpenError) -> String {
    match error {
        LibraryOpenError::CreateDirectory { path, source } => {
            format!("{} could not be created: {source}", path.display())
        }
        LibraryOpenError::Compiler { detail } => format!("the compiler host refused: {detail}"),
        LibraryOpenError::Shelf(_) => "the shelf store could not be opened".to_owned(),
        LibraryOpenError::Epoch(_) => "the library epoch file could not be read".to_owned(),
    }
}

fn coordinates(shelf: Option<&Shelf>) -> Vec<PackageCoordinate> {
    shelf.map_or_else(Vec::new, |shelf| {
        shelf.entries.iter().map(|entry| entry.coordinate.clone()).collect()
    })
}

/// The width to lay out for. Nothing in this workspace may add a dependency and `unsafe` is
/// denied, so there is no `ioctl` to ask: `COLUMNS` is the one honest source, and a fixed default
/// is better than a measurement this process cannot take.
fn terminal_width() -> Width {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|text| text.trim().parse::<u16>().ok())
        .map_or(Width::DEFAULT, Width::new)
}

fn palette_for(format: Format, colour: ColourChoice) -> Palette {
    if format != Format::Human
        || colour == ColourChoice::Never
        || std::env::var_os("NO_COLOR").is_some()
        || !io::stdout().is_terminal()
    {
        Palette::plain()
    } else {
        Palette::ansi()
    }
}

fn now() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| Timestamp(0), |since| Timestamp(since.as_secs()))
}

fn emit(text: &str) -> ExitCode {
    let mut stream = io::stdout();
    if stream.write_all(text.as_bytes()).is_err() || stream.flush().is_err() {
        ExitCode::from(PROCESS_EXIT)
    } else {
        ExitCode::SUCCESS
    }
}

fn fail(detail: &str) -> ExitCode {
    let mut stream = io::stderr();
    let _ = writeln!(stream, "\u{2717} {detail}");
    ExitCode::from(PROCESS_EXIT)
}

#[cfg(test)]
mod tests {
    use interface_core::PackageEcosystem;

    use super::*;

    fn coordinate(name: &str, version: &str) -> Option<PackageCoordinate> {
        PackageCoordinate::new(PackageEcosystem::Cargo, name, version).ok()
    }

    fn shelf() -> Vec<PackageCoordinate> {
        [("serde", "1.0.196"), ("serde", "1.0.210"), ("tokio", "1.35.0")]
            .into_iter()
            .filter_map(|(name, version)| coordinate(name, version))
            .collect()
    }

    #[test]
    fn a_bare_name_resolves_against_the_shelf() {
        let shelf = vec![coordinate("tokio", "1.35.0")].into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(
            resolve_scope_entry("cargo:tokio", &shelf).ok().map(|one| one.to_string()),
            Some("cargo:tokio@1.35.0".to_owned())
        );
        assert_eq!(
            resolve_scope_entry("tokio", &shelf).ok().map(|one| one.to_string()),
            Some("cargo:tokio@1.35.0".to_owned())
        );
    }

    #[test]
    fn a_pinned_coordinate_is_taken_exactly_as_written() {
        assert_eq!(
            resolve_scope_entry("cargo:serde@1.0.196", &[]).ok().map(|one| one.to_string()),
            Some("cargo:serde@1.0.196".to_owned())
        );
    }

    #[test]
    fn several_versions_are_a_usage_error_that_lists_them() {
        let error = resolve_scope_entry("cargo:serde", &shelf()).err();
        assert_eq!(error.as_ref().map(|error| error.kind.slug()), Some("bad-value"));
        assert_eq!(error.as_ref().map(|error| error.token.clone()), Some("cargo:serde".to_owned()));
        assert_eq!(
            error.and_then(|error| match error.kind {
                UsageKind::BadValue { accepted } => Some(accepted),
                _ => None,
            }),
            Some(vec!["cargo:serde@1.0.196".to_owned(), "cargo:serde@1.0.210".to_owned()])
        );
    }

    #[test]
    fn a_name_no_shelf_row_carries_lists_what_is_there() {
        let error = resolve_scope_entry("axum", &shelf()).err();
        assert_eq!(error.as_ref().map(|error| error.kind.slug()), Some("bad-value"));
        assert_eq!(
            error.and_then(|error| match error.kind {
                UsageKind::BadValue { accepted } => Some(accepted.len()),
                _ => None,
            }),
            Some(3)
        );
    }

    #[test]
    fn only_health_and_an_answered_reply_exit_zero() {
        assert!(!carries_failure(&Reply::Removed(RemoveOutcome::Removed)));
        assert!(carries_failure(&Reply::Removed(RemoveOutcome::Absent)));
        assert!(carries_failure(&Reply::Removed(RemoveOutcome::Busy)));
    }

    #[test]
    fn the_early_scan_finds_the_format_before_the_parser_runs() {
        let arguments = ["--format", "json", "pakages"].map(str::to_owned).to_vec();
        assert_eq!(early_globals(&arguments).0, Format::Json);
        let joined = ["pakages", "--format=json", "--no-color"].map(str::to_owned).to_vec();
        assert_eq!(early_globals(&joined), (Format::Json, ColourChoice::Never));
    }
}
