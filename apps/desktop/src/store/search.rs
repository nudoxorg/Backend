//! The omnibar: one field that searches, scopes, or becomes a command palette.
//! The mode is a property of the text, not of a toggle the reader must find.
//! Every result page carries the honest lane coverage that produced it.
//!
//! Three modes share one field because they share one question — "what do I
//! want to reach?" — and switching between them should cost a single character.
//! A leading `>` means the answer is a command; a leading `@name ` narrows the
//! answer to one project; anything else searches declarations. The palette is
//! projected from the library's own command registry, so its titles and
//! descriptions are the same words the CLI and MCP surfaces use.

use super::events::SearchEvent;
use super::service::{Endpoint, Outcome, Request};
use crate::presentation::fault::{self, Fault, Operand};
use crate::presentation::identity::Identity;
use crate::presentation::signature::Signature;
use crate::presentation::status::{self, CapabilityChip, LaneChip};
use backend_library::{
    COMMANDS, CommandDomain, CommandId, CommandSpec, DeclarationKind, QueryLimit, RowId, SymbolKey,
};
use gpui::AppContext as _;
use gpui::{Context, EventEmitter, Task};
use std::time::Duration;

/// How long the field waits after a keystroke before asking the service.
const DEBOUNCE: Duration = Duration::from_millis(120);

/// Character budget for a result row's signature line.
const PREVIEW: usize = 120;

/// What the text in the field means.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    /// Search declarations across the whole shelf.
    Search,
    /// Run one product command.
    Palette,
    /// Search declarations inside one project.
    Scoped {
        /// Project name the reader typed after `@`.
        project: String,
    },
}

impl Mode {
    /// Returns the placeholder shown for this mode.
    pub(crate) const fn placeholder(&self) -> &'static str {
        match self {
            Self::Search => "Search declarations…  >  for commands",
            Self::Palette => "Run a command…",
            Self::Scoped { .. } => "Search inside this project…",
        }
    }
}

/// The text split into a mode and the term that mode should act on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Parsed {
    mode: Mode,
    term: String,
}

impl Parsed {
    /// Splits raw field text into a mode and a term.
    pub(crate) fn of(text: &str) -> Self {
        if let Some(rest) = text.strip_prefix('>') {
            return Self {
                mode: Mode::Palette,
                term: rest.trim_start().to_owned(),
            };
        }
        if let Some(rest) = text.strip_prefix('@') {
            let (project, term) = rest.split_once(' ').unwrap_or((rest, ""));
            if !project.is_empty() {
                return Self {
                    mode: Mode::Scoped {
                        project: project.to_owned(),
                    },
                    term: term.trim_start().to_owned(),
                };
            }
        }
        Self {
            mode: Mode::Search,
            term: text.to_owned(),
        }
    }

    /// Returns the detected mode.
    pub(crate) const fn mode(&self) -> &Mode {
        &self.mode
    }

    /// Returns the term the mode should act on.
    pub(crate) fn term(&self) -> &str {
        &self.term
    }
}

/// One declaration the service returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResultRow {
    id: RowId,
    symbol: Option<SymbolKey>,
    identity: Identity,
    kind: Option<DeclarationKind>,
    preview: String,
}

impl ResultRow {
    /// Returns the stable row identity.
    pub(crate) const fn id(&self) -> RowId {
        self.id
    }

    /// Returns the declaration this row opens.
    pub(crate) const fn symbol(&self) -> Option<SymbolKey> {
        self.symbol
    }

    /// Returns the row's identity.
    pub(crate) const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns the typed declaration kind.
    pub(crate) const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the one-line signature preview.
    pub(crate) fn preview(&self) -> &str {
        &self.preview
    }
}

/// One command the palette can run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandRow {
    spec: CommandSpec,
}

impl CommandRow {
    /// Returns the registry row, whose words every surface shares.
    pub(crate) const fn spec(self) -> CommandSpec {
        self.spec
    }

    /// Returns the domain header this command groups under.
    pub(crate) const fn domain(self) -> CommandDomain {
        self.spec.domain
    }
}

/// What the palette produced for the command it last ran.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Reply {
    /// Nothing has been run.
    Idle,
    /// A command is on the wire.
    Running(CommandId),
    /// Capability rows, shown in place.
    Capabilities(Vec<CapabilityChip>),
    /// Plain result lines, shown in place.
    Lines(Vec<String>),
    /// The command needs an argument this field cannot supply yet.
    Guidance(String),
    /// The command failed.
    Faulted(Box<Fault>),
}

/// The omnibar's whole state.
pub(crate) struct SearchStore {
    endpoint: Endpoint,
    text: String,
    parsed: Parsed,
    results: Vec<ResultRow>,
    commands: Vec<CommandRow>,
    coverage: Vec<LaneChip>,
    selected: usize,
    open: bool,
    searching: bool,
    fault: Option<Fault>,
    reply: Reply,
    pending: Option<Task<()>>,
    generation: u64,
}

impl EventEmitter<SearchEvent> for SearchStore {}

impl SearchStore {
    /// Creates a closed omnibar bound to one endpoint.
    pub(crate) fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            text: String::new(),
            parsed: Parsed::of(""),
            results: Vec::new(),
            commands: palette_rows(""),
            coverage: Vec::new(),
            selected: 0,
            open: false,
            searching: false,
            fault: None,
            reply: Reply::Idle,
            pending: None,
            generation: 0,
        }
    }

    /// Returns the raw field text.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Returns the parsed mode and term.
    pub(crate) const fn parsed(&self) -> &Parsed {
        &self.parsed
    }

    /// Returns the declaration results.
    pub(crate) fn results(&self) -> &[ResultRow] {
        &self.results
    }

    /// Returns the matching commands, in registry order.
    pub(crate) fn commands(&self) -> &[CommandRow] {
        &self.commands
    }

    /// Returns the honest lane coverage behind the current results.
    pub(crate) fn coverage(&self) -> &[LaneChip] {
        &self.coverage
    }

    /// Returns the selected row index.
    pub(crate) const fn selected(&self) -> usize {
        self.selected
    }

    /// Returns whether the sheet is dropped.
    pub(crate) const fn is_open(&self) -> bool {
        self.open
    }

    /// Returns whether a query is on the wire.
    pub(crate) const fn is_searching(&self) -> bool {
        self.searching
    }

    /// Returns the search fault, when the last query failed.
    pub(crate) const fn fault(&self) -> Option<&Fault> {
        self.fault.as_ref()
    }

    /// Returns what the palette's last command produced.
    pub(crate) const fn reply(&self) -> &Reply {
        &self.reply
    }

    /// Returns how many rows the sheet currently lists.
    pub(crate) fn len(&self) -> usize {
        match self.parsed.mode {
            Mode::Palette => self.commands.len(),
            _ => self.results.len(),
        }
    }

    /// Opens the sheet without changing the text.
    pub(crate) fn open(&mut self, cx: &mut Context<Self>) {
        if self.open {
            return;
        }
        self.open = true;
        cx.emit(SearchEvent::Changed);
        cx.notify();
    }

    /// Closes the sheet and clears any transient reply.
    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.reply = Reply::Idle;
        cx.emit(SearchEvent::Dismissed);
        cx.notify();
    }

    /// Replaces the field text and re-runs whatever the new text means.
    pub(crate) fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        if self.text == text {
            return;
        }
        self.text = text;
        self.parsed = Parsed::of(&self.text);
        self.selected = 0;
        self.reply = Reply::Idle;
        self.open = !self.text.is_empty();
        match self.parsed.mode {
            Mode::Palette => self.refresh_palette(cx),
            Mode::Search | Mode::Scoped { .. } => self.refresh_results(cx),
        }
        cx.emit(SearchEvent::Changed);
        cx.notify();
    }

    /// Moves the selection by one row, clamped to the list.
    pub(crate) fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.len();
        if len == 0 {
            return;
        }
        let last = len.saturating_sub(1);
        self.selected = match delta {
            step if step < 0 => self.selected.saturating_sub(step.unsigned_abs()),
            step => self.selected.saturating_add(step.unsigned_abs()).min(last),
        };
        cx.emit(SearchEvent::Changed);
        cx.notify();
    }

    /// Moves the selection to an absolute row.
    pub(crate) fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let last = self.len().saturating_sub(1);
        self.selected = index.min(last);
        cx.emit(SearchEvent::Changed);
        cx.notify();
    }

    /// Returns the selected declaration, when the sheet is listing results.
    pub(crate) fn selected_result(&self) -> Option<&ResultRow> {
        match self.parsed.mode {
            Mode::Palette => None,
            _ => self.results.get(self.selected),
        }
    }

    /// Returns the selected command, when the sheet is a palette.
    pub(crate) fn selected_command(&self) -> Option<CommandRow> {
        match self.parsed.mode {
            Mode::Palette => self.commands.get(self.selected).copied(),
            _ => None,
        }
    }

    fn refresh_palette(&mut self, cx: &mut Context<Self>) {
        self.pending = None;
        self.searching = false;
        self.fault = None;
        self.commands = palette_rows(self.parsed.term());
        cx.notify();
    }

    fn refresh_results(&mut self, cx: &mut Context<Self>) {
        let term = self.parsed.term().trim().to_owned();
        if term.is_empty() {
            self.pending = None;
            self.searching = false;
            self.results.clear();
            self.coverage.clear();
            self.fault = None;
            return;
        }
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        let endpoint = self.endpoint.clone();
        let scope = match &self.parsed.mode {
            Mode::Scoped { project } => Some(project.clone()),
            _ => None,
        };
        self.searching = true;
        self.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let request = Request::Search {
                text: term.clone(),
                limit: QueryLimit::new(QueryLimit::MAX).unwrap_or_default(),
            };
            let outcome = cx
                .background_spawn(async move { super::service::run(endpoint.path(), &request) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.install(generation, &term, scope.as_deref(), outcome, cx);
            });
        }));
    }

    fn install(
        &mut self,
        generation: u64,
        term: &str,
        scope: Option<&str>,
        outcome: Result<Outcome, backend_client::ClientError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        self.searching = false;
        match outcome {
            Ok(Outcome::Rows(page)) => {
                self.coverage = status::lane_chips(page.coverage());
                self.fault = None;
                self.results = page
                    .into_rows()
                    .iter()
                    .filter_map(|row| result_row(row))
                    .filter(|row| scope.is_none_or(|name| row.identity.project_name() == name))
                    .collect();
                self.selected = 0;
            }
            Ok(_) => self.fault = Some(shape_fault(term)),
            Err(error) => {
                self.results.clear();
                self.coverage.clear();
                self.fault = Some(fault::from_client(
                    &error,
                    Operand::Query {
                        text: term.to_owned(),
                    },
                ));
            }
        }
        cx.emit(SearchEvent::Changed);
        cx.notify();
    }
}

/// Running a command from the palette.
impl SearchStore {
    /// Runs one registry command and shows its typed reply inside the sheet.
    pub(crate) fn run(&mut self, spec: CommandSpec, cx: &mut Context<Self>) {
        let Some(request) = argument_free_request(spec) else {
            self.reply = Reply::Guidance(guidance_for(spec));
            cx.emit(SearchEvent::Accepted);
            cx.notify();
            return;
        };
        self.reply = Reply::Running(spec.id);
        cx.emit(SearchEvent::Accepted);
        cx.notify();
        let endpoint = self.endpoint.clone();
        self.pending = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move { super::service::run(endpoint.path(), &request) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.reply = reply_of(spec, outcome);
                cx.notify();
            });
        }));
    }
}

fn reply_of(spec: CommandSpec, outcome: Result<Outcome, backend_client::ClientError>) -> Reply {
    let operand = Operand::Capability {
        name: spec.title.to_owned(),
    };
    match outcome {
        Ok(Outcome::Health(report)) => {
            Reply::Capabilities(status::capability_chips(report.capabilities()))
        }
        Ok(Outcome::Rows(page)) => Reply::Lines(
            page.rows()
                .iter()
                .map(|row| Identity::parse(&row.label).compact())
                .collect(),
        ),
        Ok(Outcome::Surface(reply)) => Reply::Lines(surface_lines(&reply)),
        Ok(_) => Reply::Lines(vec![format!("{} completed.", spec.title)]),
        Err(error) => Reply::Faulted(Box::new(fault::from_client(&error, operand))),
    }
}

fn surface_lines(reply: &backend_library::SurfaceReply) -> Vec<String> {
    match reply {
        backend_library::SurfaceReply::Projects(rows) => rows
            .iter()
            .map(|row| format!("{} · {} members", row.name.as_str(), row.members.len()))
            .collect(),
        backend_library::SurfaceReply::Subscriptions(rows) => rows
            .iter()
            .map(|row| row.package.as_str().to_owned())
            .collect(),
        backend_library::SurfaceReply::Releases(rows) => rows
            .iter()
            .map(|row| format!("{} {}", row.package.as_str(), row.version.as_str()))
            .collect(),
        backend_library::SurfaceReply::Tree(rows) => rows
            .iter()
            .map(|row| row.title.as_str().to_owned())
            .collect(),
        other => vec![format!("{:?} returned no listable rows.", other.id())],
    }
}

/// Returns the request for a command that needs no argument to be useful.
///
/// Commands that need an operand are not run blind: the palette explains what
/// they want instead, which is honest and also keeps a stray Return from
/// mutating durable state.
fn argument_free_request(spec: CommandSpec) -> Option<Request> {
    let command = match spec.id {
        CommandId::Health | CommandId::Revision => return Some(Request::Health),
        CommandId::Packages => {
            return Some(Request::Search {
                text: String::new(),
                limit: QueryLimit::default(),
            });
        }
        CommandId::Projects => backend_library::SurfaceCommand::Projects,
        CommandId::Subscriptions => backend_library::SurfaceCommand::Subscriptions,
        CommandId::Releases => backend_library::SurfaceCommand::Releases { mark_seen: false },
        CommandId::Tree => backend_library::SurfaceCommand::Tree,
        _ => return None,
    };
    Some(Request::Surface {
        command: Box::new(command),
    })
}

fn guidance_for(spec: CommandSpec) -> String {
    format!(
        "{} needs an operand. {} Run it from the row it applies to, or from the CLI as `backend {}`.",
        spec.title, spec.description, spec.name
    )
}

fn palette_rows(term: &str) -> Vec<CommandRow> {
    let needle = term.trim().to_ascii_lowercase();
    COMMANDS
        .into_iter()
        .filter(|spec| matches_command(*spec, &needle))
        .map(|spec| CommandRow { spec })
        .collect()
}

fn matches_command(spec: CommandSpec, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    spec.name.to_ascii_lowercase().contains(needle)
        || spec.title.to_ascii_lowercase().contains(needle)
        || spec.description.to_ascii_lowercase().contains(needle)
}

fn result_row(row: &backend_library::Row) -> Option<ResultRow> {
    let identity = Identity::parse(&row.label);
    let preview = row.signature.as_deref().map_or_else(
        || identity.compact(),
        |text| Signature::parse(text, identity.name()).preview(PREVIEW),
    );
    Some(ResultRow {
        id: row.id,
        symbol: match row.id {
            RowId::Symbol(symbol) => Some(symbol),
            _ => None,
        },
        kind: row.kind,
        identity,
        preview,
    })
}

fn shape_fault(term: &str) -> Fault {
    Fault::new(
        fault::Severity::Fault,
        Operand::Query {
            text: term.to_owned(),
        },
        "The search reply changed shape",
        "the service answered a search with a reply this build cannot admit",
        vec![fault::Affordance::Retry],
    )
}
