//! Defines markdown behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the markdown invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One [`Reply`] rendered as the Markdown an agent reads, built on [`interface_documents::walk_page`].
//!
//! Three rules shape every line here. An address is never placed in a table cell, because cell
//! escaping corrupts the lifetimes and union pipes a reader copies back. A page's coordinate is
//! written once, in its heading, and every target below it is spelled by
//! [`common::RelativeAddress`]. Every failure is a [`common::Fault`] whose affordance is the exact
//! `tools/call` arguments that could make progress, so a refusal is a next step rather than an
//! apology.

use core::fmt::Write as _;

use compiler_ir::Confidence;
use compiler_vocabulary::Language;
use interface_documents::{
    Block, Direction, Inline, MemberGroup, Outline, Page, PageVisitor, RelationGroup, Signature,
    SourceLocation, Symbol, walk_page,
};
use interface_identity::{KindTag, PackageCoordinate};
use interface_search::{
    Coverage, GraphTerminal, KindSet, SearchTerminal, Truncation, relation_label,
};

use crate::{
    AddFailure, AddOutcome, AddRejection, Health, PageError, RejectedAdd, RemoveOutcome, Reply,
    Resolution, ResolveError, Shelf, ShelfEntry, ShelfError, ShelfStatus,
    render::common::{
        self, Affordance, Affordances, EXAMPLE_PACKAGE_URL, Fault, RenderContext, RelativeAddress,
        add_affordance, blamed_capability, capability_glyph, capability_slug, census_line,
        fence_tag, lane_signal, package_url_slug, phase_dots, rejection_slug,
        relative_age, shelf_failure_slug, unavailability_slug, visibility_label, write_affordance,
        write_fault,
    },
};

/// Most member rows one page spells before it stops being a page and becomes a dump.
///
/// A projection may retain thousands; a reader who needs all of them wants `outline`, which is why
/// the truncation line offers exactly that call.
pub const MAX_RENDERED_MEMBERS: usize = 120;
/// Most rows one relation group spells before the group is summarised.
pub const MAX_RENDERED_RELATIONS: usize = 10;
/// Most nodes one outline spells before the tree is summarised.
pub const MAX_RENDERED_OUTLINE_NODES: usize = 400;

/// Renders one reply as the Markdown an agent reads.
#[must_use]
pub fn render(reply: &Reply, context: &RenderContext) -> String {
    match reply {
        Reply::Packages(result) => match result {
            Ok(shelf) => shelf_markdown(shelf, context),
            Err(error) => fault_markdown("packages", &shelf_error_fault(error)),
        },
        Reply::Added(outcome) => added_markdown(outcome),
        Reply::Removed(outcome) => removed_markdown(outcome),
        Reply::Page(result) => match result {
            Ok(page) => page_markdown(page),
            Err(error) => page_error_markdown("show", error),
        },
        Reply::Outline(result) => match result {
            Ok(outline) => outline_markdown(outline),
            Err(error) => page_error_markdown("outline", error),
        },
        Reply::Resolved(result) => match result {
            Ok(resolution) => resolution_markdown(resolution),
            Err(error) => fault_markdown("resolve", &resolve_error_fault(error)),
        },
        Reply::Searched(terminal) => search_markdown(terminal),
        Reply::Graphed(result) => match result {
            Ok(terminal) => graph_markdown(terminal),
            Err(error) => page_error_markdown("graph", error),
        },
        Reply::Health(health) => health_markdown(health),
    }
}

/// Spells an affordance as the exact `tools/call` arguments an agent sends next.
///
/// Public because the MCP server renders faults that never reached the library — a misspelled
/// argument, an unknown resource — and a reader must not be able to tell which layer refused.
#[derive(Clone, Copy, Debug, Default)]
pub struct AgentAffordances;

impl Affordances for AgentAffordances {
    fn spell(&self, affordance: &Affordance) -> Option<String> {
        Some(match affordance {
            Affordance::None => return None,
            Affordance::Packages => "packages {}".to_owned(),
            Affordance::Health => "health {}".to_owned(),
            Affordance::Add { package } => {
                format!("add {{\"package\":{}}}", json_string(package))
            }
            Affordance::Resolve { text } => format!("resolve {{\"text\":{}}}", json_string(text)),
            Affordance::Search { query } => format!("search {{\"query\":{}}}", json_string(query)),
            Affordance::Show { address } => {
                format!("show {{\"address\":{}}}", json_string(address))
            }
        })
    }
}

/// Quotes one string as a JSON scalar, escaping exactly what the grammar requires.
fn json_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            control if control < ' ' => {
                let _ = write!(quoted, "\\u{:04x}", u32::from(control));
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

fn heading(out: &mut String, title: &str) {
    out.push_str("# ");
    out.push_str(title);
    out.push_str("\n\n");
}

fn fault_markdown(title: &str, fault: &Fault) -> String {
    let mut out = String::new();
    heading(&mut out, title);
    write_fault(&mut out, fault, &AgentAffordances);
    out
}

/// Renders one fault on its own, for refusals that never reached a command.
#[must_use]
pub fn render_fault(fault: &Fault) -> String {
    let mut out = String::new();
    write_fault(&mut out, fault, &AgentAffordances);
    out
}

/// The longest run of backticks inside `text`, so a fence can always be one longer.
fn fence_for(text: &str) -> String {
    let mut longest = 0_usize;
    let mut run = 0_usize;
    for byte in text.bytes() {
        if byte == b'`' {
            run = run.saturating_add(1);
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.saturating_add(1).max(3))
}

fn write_fenced(out: &mut String, language: Language, text: &str) {
    let fence = fence_for(text);
    out.push_str(&fence);
    out.push_str(fence_tag(language));
    out.push('\n');
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push_str("\n\n");
}

// ── packages ────────────────────────────────────────────────────────────────────────────────────

fn shelf_markdown(shelf: &Shelf, context: &RenderContext) -> String {
    let mut out = String::new();
    heading(&mut out, "packages");
    if shelf.entries.is_empty() {
        out.push_str("(no packages)\n");
        write_affordance(
            &mut out,
            &Affordance::Add {
                package: EXAMPLE_PACKAGE_URL.to_owned(),
            },
            &AgentAffordances,
        );
        return out;
    }
    for entry in &shelf.entries {
        write_shelf_entry(&mut out, entry, context);
    }
    out
}

fn write_shelf_entry(out: &mut String, entry: &ShelfEntry, context: &RenderContext) {
    match &entry.status {
        ShelfStatus::Requested => {
            let _ = writeln!(out, "· {}  requested", entry.coordinate);
        }
        ShelfStatus::Compiling { phase } => {
            let _ = writeln!(
                out,
                "◐ {}  {} {}",
                entry.coordinate,
                phase_dots(*phase),
                crate::CompilePhaseProgress::of(*phase).label()
            );
        }
        ShelfStatus::Ready { card } => {
            let _ = writeln!(
                out,
                "✓ {}  {}  {}",
                card.coordinate,
                census_line(&card.census),
                relative_age(context.now, card.published_at)
            );
        }
        ShelfStatus::Failed { cause } => {
            let _ = writeln!(
                out,
                "✗ {}  {}",
                entry.coordinate,
                shelf_failure_slug(cause)
            );
            if let crate::ShelfFailure::Compiler { summary } = cause {
                let _ = writeln!(out, "  {summary}");
            }
        }
    }
}

fn shelf_error_fault(error: &ShelfError) -> Fault {
    // A shelf that cannot be read is a capability failure, never an empty shelf: rendering zero
    // packages here would be the one lie this layer refuses to tell.
    match error {
        ShelfError::Store { detail } => {
            Fault::new("shelf-unreadable", String::new(), Affordance::Health)
                .detailed(detail.as_ref())
        }
        ShelfError::Capacity { maximum } => {
            Fault::new("shelf-full", String::new(), Affordance::Packages)
                .detailed(format!("the shelf holds its fixed maximum of {maximum} packages"))
        }
        ShelfError::Epoch(_) => Fault::new("epoch-unreadable", String::new(), Affordance::Health)
            .detailed("the library epoch file could not be read, so no consistent shelf read exists"),
    }
}

// ── add and remove ──────────────────────────────────────────────────────────────────────────────

fn added_markdown(outcome: &AddOutcome) -> String {
    let mut out = String::new();
    heading(&mut out, "add");
    match outcome {
        AddOutcome::Ready { card } => {
            let _ = writeln!(
                out,
                "✓ {}  {}",
                card.coordinate,
                census_line(&card.census)
            );
            write_affordance(
                &mut out,
                &Affordance::Show {
                    address: card.coordinate.to_string(),
                },
                &AgentAffordances,
            );
        }
        AddOutcome::Rejected(rejected) => write_fault(&mut out, &rejection_fault(rejected), &AgentAffordances),
        AddOutcome::Failed(failure) => write_fault(&mut out, &failure_fault(failure), &AgentAffordances),
    }
    out
}

fn rejection_fault(rejected: &RejectedAdd) -> Fault {
    let operand: &str = rejected.url.as_ref();
    let slug = rejection_slug(&rejected.rejection);
    match &rejected.rejection {
        AddRejection::PackageUrl { cause } => {
            Fault::new(slug, operand, Affordance::None).detailed(format!(
                "the {} part of this package url was rejected",
                package_url_slug(*cause)
            ))
        }
        AddRejection::CompilerDetached => Fault::new(slug, operand, Affordance::Health)
            .detailed("this process opened the library without a compiler, so nothing can be added from it"),
        AddRejection::Busy { active } => Fault::new(slug, operand, Affordance::Packages).detailed(
            match active {
                Some(coordinate) => format!("another process is compiling {coordinate}"),
                None => "another process holds the compile lock".to_owned(),
            },
        ),
        AddRejection::AlreadyReady => Fault::new(slug, operand, Affordance::Packages)
            .detailed("this package is already on the shelf and readable"),
        AddRejection::ShelfFull { maximum } => Fault::new(slug, operand, Affordance::Packages)
            .detailed(format!("the shelf holds its fixed maximum of {maximum} packages")),
    }
}

fn failure_fault(failure: &AddFailure) -> Fault {
    // `AddFailure` retains the correlation and the cause but not the coordinate, so the operand is
    // the correlation: the shelf row it names carries the package.
    let fault = Fault::new(
        shelf_failure_slug(&failure.cause),
        format!("correlation {}", failure.correlation.0),
        Affordance::Packages,
    );
    match &failure.cause {
        crate::ShelfFailure::Compiler { summary } => fault.detailed(summary.as_ref()),
        _ => fault,
    }
}

fn removed_markdown(outcome: &RemoveOutcome) -> String {
    let mut out = String::new();
    heading(&mut out, "remove");
    match outcome {
        RemoveOutcome::Removed => out.push_str("✓ removed\n"),
        RemoveOutcome::Absent => out.push_str("· absent — nothing changed\n"),
        RemoveOutcome::Busy => write_fault(
            &mut out,
            &Fault::new("compiler-busy", String::new(), Affordance::Packages)
                .detailed("a live process is compiling this package; it was left alone"),
            &AgentAffordances,
        ),
    }
    out
}

// ── page ────────────────────────────────────────────────────────────────────────────────────────

/// Renders one page by walking it, so Markdown and terminal output cannot disagree about content.
///
/// Sections are buffered rather than streamed because the meta line belongs directly beneath the
/// heading while [`walk_page`] delivers the source location last. Buffering keeps every callback
/// contributing to the output instead of reaching around the walk for facts it already delivers.
struct PageMarkdown<'page> {
    language: Language,
    home: &'page PackageCoordinate,
    header: Option<Symbol>,
    source: Option<SourceLocation>,
    body: String,
    members_written: usize,
    members_open: bool,
}

impl<'page> PageMarkdown<'page> {
    fn new(page: &'page Page) -> Self {
        Self {
            language: page.language,
            home: page.symbol.package(),
            header: None,
            source: None,
            body: String::new(),
            members_written: 0,
            members_open: false,
        }
    }

    const fn relative(&self) -> RelativeAddress<'page> {
        RelativeAddress::to(self.home)
    }

    fn close_members(&mut self) {
        if !self.members_open {
            return;
        }
        self.members_open = false;
        if let Some(symbol) = &self.header {
            let _ = writeln!(
                self.body,
                "\n*members are addressable as {}::{{name}}*\n",
                symbol.address
            );
        }
    }

    fn finish(mut self, attributes: &[interface_documents::Text]) -> String {
        self.close_members();
        let mut out = String::new();
        let Some(symbol) = self.header.as_ref() else {
            return out;
        };
        heading(&mut out, &symbol.address.to_string());
        out.push_str(&meta_line(symbol, self.source.as_ref()));
        out.push('\n');
        if !attributes.is_empty() {
            let spellings: Vec<&str> = attributes
                .iter()
                .map(interface_documents::Text::as_str)
                .collect();
            let _ = writeln!(out, "{}", spellings.join(" "));
        }
        out.push('\n');
        out.push_str(&self.body);
        out
    }
}

/// `fn · public · <full key> · src/de/mod.rs:41203..41876`, the one place the full key appears.
fn meta_line(symbol: &Symbol, source: Option<&SourceLocation>) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(4);
    parts.push(KindTag::of(symbol.kind).as_str().to_owned());
    if let Some(label) = visibility_label(symbol.visibility) {
        parts.push(label.to_owned());
    }
    parts.push(symbol.key().to_string());
    if let Some(location) = source {
        parts.push(format!(
            "{}:{}..{}",
            location.file,
            location.span.start.0,
            location.span.end.0
        ));
    }
    parts.join(" · ")
}

impl PageVisitor for PageMarkdown<'_> {
    fn header(&mut self, symbol: &Symbol) {
        // Retained rather than borrowed: the heading and the members footer both spell this
        // address, and the walk hands elements out with a lifetime it does not tie to the page.
        self.header = Some(symbol.clone());
    }

    fn trail(&mut self, _crumbs: &[Symbol]) {
        // Deliberately nothing. The heading is the full address, which is the trail spelled once;
        // a second `↑ … › de[mod] › Deserializer[trait]` line repeats it in a weaker notation.
    }

    fn signature(&mut self, signature: &Signature) {
        self.close_members();
        let plain = signature.plain();
        write_fenced(&mut self.body, self.language, &plain);
    }

    fn prose_block(&mut self, block: &Block) {
        self.close_members();
        match block {
            Block::Paragraph(inlines) => {
                let text = inline_run(inlines);
                if !text.is_empty() {
                    self.body.push_str(&text);
                    self.body.push_str("\n\n");
                }
            }
            Block::Code(text) => write_fenced(&mut self.body, self.language, text.as_str()),
        }
    }

    fn members(&mut self, group: &MemberGroup) {
        if !self.members_open {
            self.members_open = true;
            self.body.push_str("## members\n");
        }
        for row in &group.rows {
            if self.members_written == MAX_RENDERED_MEMBERS {
                let _ = writeln!(
                    self.body,
                    "… more members — the whole tree is one `outline` call away"
                );
                return;
            }
            self.members_written = self.members_written.saturating_add(1);
            let spelling = if row.signature.is_empty() {
                format!(
                    "{} {}",
                    KindTag::of(row.symbol.kind).as_str(),
                    row.symbol.name
                )
            } else {
                row.signature.plain()
            };
            let _ = writeln!(
                self.body,
                "- {spelling}  ~{}",
                row.symbol.key().abbreviation()
            );
        }
    }

    fn relations(&mut self, group: &RelationGroup) {
        self.close_members();
        let relative = self.relative();
        let _ = writeln!(
            self.body,
            "## {}",
            relation_label(group.role.kind, group.role.direction)
        );
        for row in group.rows.iter().take(MAX_RENDERED_RELATIONS) {
            let _ = writeln!(
                self.body,
                "- {}{}",
                relative.target(&row.target),
                confidence_suffix(row.confidence)
            );
        }
        let hidden = group.rows.len().saturating_sub(MAX_RENDERED_RELATIONS);
        if hidden != 0 {
            let _ = writeln!(self.body, "… +{hidden}");
        }
        self.body.push('\n');
    }

    fn source(&mut self, location: &SourceLocation) {
        self.source = Some(location.clone());
    }
}

const fn confidence_suffix(confidence: Confidence) -> &'static str {
    // A compiler-proved edge is the expected case and says nothing worth a word; anything weaker
    // is a claim the reader should weigh, so only the weaker cases spend tokens.
    match confidence {
        Confidence::Compiler => "",
        Confidence::Syntactic => "  syntactic",
        Confidence::Heuristic => "  heuristic",
        Confidence::Indexed => "  indexed",
        Confidence::Imported => "  imported",
    }
}

/// Renders one prose run. Links render as their label in code style: the resolved graph belongs in
/// the relations block, not smuggled into a sentence as a second address spelling.
fn inline_run(inlines: &[Inline]) -> String {
    let mut text = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(run) => text.push_str(run.as_str()),
            Inline::Code(run) => {
                let _ = write!(text, "`{run}`");
            }
            Inline::Link { label, .. } => {
                let _ = write!(text, "`{label}`");
            }
            Inline::Break => text.push('\n'),
        }
    }
    text.trim().to_owned()
}

fn page_markdown(page: &Page) -> String {
    let mut visitor = PageMarkdown::new(page);
    walk_page(page, &mut visitor);
    visitor.finish(&page.attributes)
}

// ── outline ─────────────────────────────────────────────────────────────────────────────────────

fn outline_markdown(outline: &Outline) -> String {
    let mut out = String::new();
    heading(&mut out, &format!("outline {}", outline.package));
    let _ = writeln!(out, "{}\n", census_line(&outline.census));
    let mut written = 0_usize;
    for (node, depth) in outline.walk() {
        if written == MAX_RENDERED_OUTLINE_NODES {
            let _ = writeln!(out, "… more declarations — narrow with `search`");
            break;
        }
        written = written.saturating_add(1);
        let _ = writeln!(
            out,
            "{}{} {}  ~{}",
            "  ".repeat(depth),
            KindTag::glyph(node.symbol.kind),
            node.symbol.name,
            node.symbol.key().abbreviation()
        );
    }
    if written == 0 {
        out.push_str("(no declarations)\n");
    }
    out
}

// ── resolve ─────────────────────────────────────────────────────────────────────────────────────

fn resolution_markdown(resolution: &Resolution) -> String {
    let mut out = String::new();
    heading(&mut out, "resolve");
    match resolution {
        Resolution::Exact(symbol) => {
            let _ = writeln!(out, "{}", symbol.address);
            write_affordance(
                &mut out,
                &Affordance::Show {
                    address: symbol.address.to_string(),
                },
                &AgentAffordances,
            );
        }
        Resolution::Ambiguous(candidates) => write_candidates(&mut out, candidates),
        Resolution::Unknown { package } => write_fault(
            &mut out,
            &Fault::new("no-match", package.to_string(), Affordance::Search {
                query: package.name.as_str().to_owned(),
            })
            .detailed("this package is loaded and holds no declaration with that path"),
            &AgentAffordances,
        ),
    }
    out
}

fn write_candidates(out: &mut String, candidates: &[Symbol]) {
    let affordance = candidates.first().map_or(Affordance::None, |first| {
        Affordance::Show {
            address: first.address.to_string(),
        }
    });
    write_fault(
        out,
        &Fault::new("ambiguous", String::new(), affordance).detailed(format!(
            "{} declarations share this spelling; pass one back",
            candidates.len()
        )),
        &AgentAffordances,
    );
    out.push('\n');
    for (index, candidate) in candidates.iter().enumerate() {
        let _ = writeln!(
            out,
            "{}. {}  {}",
            index.saturating_add(1),
            candidate.address,
            KindTag::of(candidate.kind).as_str()
        );
    }
}

fn resolve_error_fault(error: &ResolveError) -> Fault {
    match error {
        ResolveError::Parse { cause } => Fault::new(
            "address",
            String::new(),
            Affordance::Packages,
        )
        .detailed(format!(
            "{} — an address is `ecosystem:name@version::path`, as in `cargo:serde@1.0.196::de::Deserializer[trait]`",
            common::address_parse_detail(*cause)
        )),
        ResolveError::PackageUnknown { package } => Fault::new(
            "package-not-on-shelf",
            package.to_string(),
            add_affordance(package),
        )
        .detailed("no shelf row names this coordinate; absent here never means absent everywhere"),
        ResolveError::PackageNotReady { package } => Fault::new(
            "package-not-ready",
            package.to_string(),
            Affordance::Packages,
        )
        .detailed("the shelf holds this package but it has no readable publication yet"),
    }
}

// ── page failures ───────────────────────────────────────────────────────────────────────────────

fn page_error_markdown(title: &str, error: &PageError) -> String {
    let mut out = String::new();
    heading(&mut out, title);
    match error {
        PageError::Ambiguous(candidates) => write_candidates(&mut out, candidates),
        PageError::Resolve(resolve) => write_fault(&mut out, &resolve_error_fault(resolve), &AgentAffordances),
        PageError::Reopen(reopen) => write_fault(
            &mut out,
            &Fault::new("image-unreadable", reopen.package.to_string(), Affordance::Health)
                .detailed(format!(
                    "{}: {}",
                    common::reopen_phase_slug(reopen.phase),
                    reopen.detail
                )),
            &AgentAffordances,
        ),
        PageError::Projection(projection) => write_fault(
            &mut out,
            &Fault::new("projection", String::new(), Affordance::Health)
                .detailed(common::projection_detail(projection)),
            &AgentAffordances,
        ),
        PageError::KeyUnknown { key } => write_fault(
            &mut out,
            &Fault::new("key-unknown", key.to_string(), Affordance::Packages)
                .detailed("no loaded package declares this key"),
            &AgentAffordances,
        ),
    }
    out
}

// ── search ──────────────────────────────────────────────────────────────────────────────────────

fn search_markdown(terminal: &SearchTerminal) -> String {
    let mut out = String::new();
    heading(
        &mut out,
        &format!("search {}", json_string(terminal.request.text.as_str())),
    );
    let signals: Vec<String> = terminal.lanes.iter().map(lane_signal).collect();
    let _ = writeln!(out, "~lanes {}", signals.join(" "));
    if let Some(scope) = scope_line(terminal) {
        let _ = writeln!(out, "~scope {scope}");
    }
    out.push('\n');
    if terminal.hits.is_empty() {
        out.push_str("(no matches)\n");
        write_affordance(&mut out, &empty_search_affordance(terminal), &AgentAffordances);
        return out;
    }
    for hit in &terminal.hits {
        let _ = writeln!(
            out,
            "{} {}  ~{}  {}",
            KindTag::glyph(hit.symbol.kind),
            hit.symbol.address,
            hit.symbol.key().abbreviation(),
            hit.lane.label()
        );
        if let Some(signature) = &hit.signature {
            let _ = writeln!(out, "  {}", signature.plain());
        }
        if let Some(summary) = &hit.summary {
            let _ = writeln!(out, "  {summary}");
        }
    }
    if let Truncation::Truncated { next } = terminal.truncation {
        let _ = writeln!(out, "\n… more results — pass cursor {}", next.0);
    }
    out
}

/// The scope line, written only when the request narrowed something the caller did not name.
fn scope_line(terminal: &SearchTerminal) -> Option<String> {
    let scope = &terminal.request.scope;
    let mut parts: Vec<String> = Vec::with_capacity(2);
    if scope.kinds != KindSet::ALL {
        let excluded: Vec<&str> = scope
            .kinds
            .excluded()
            .map(|kind| KindTag::of(kind).as_str())
            .collect();
        if !excluded.is_empty() {
            parts.push(format!("kinds exclude {}", excluded.join(" ")));
        }
    }
    if let Some(packages) = &scope.packages {
        let names: Vec<String> = packages.iter().map(PackageCoordinate::to_string).collect();
        parts.push(format!("packages {}", names.join(" ")));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// What a reader with zero rows should do, decided by why the lanes were empty rather than guessed.
fn empty_search_affordance(terminal: &SearchTerminal) -> Affordance {
    let mut blamed = None;
    for report in &terminal.lanes {
        if let Coverage::Unavailable { reason } = report.coverage {
            if matches!(reason, interface_search::Unavailability::NoPackages) {
                return Affordance::Add {
                    package: EXAMPLE_PACKAGE_URL.to_owned(),
                };
            }
            if blamed.is_none() {
                blamed = blamed_capability(reason);
            }
        }
    }
    if blamed.is_some() {
        return Affordance::Health;
    }
    Affordance::Packages
}

// ── graph ───────────────────────────────────────────────────────────────────────────────────────

fn graph_markdown(terminal: &GraphTerminal) -> String {
    let mut out = String::new();
    heading(&mut out, &format!("graph {}", terminal.source.address));
    let _ = writeln!(out, "~graph {}\n", coverage_signal(terminal.coverage));
    if terminal.edges.is_empty() {
        out.push_str("(no relations)\n");
        write_affordance(&mut out, &Affordance::Health, &AgentAffordances);
        return out;
    }
    let relative = RelativeAddress::to(terminal.source.package());
    let mut current: Option<&'static str> = None;
    for edge in &terminal.edges {
        // Direction is derived, never assumed: an edge whose source is the requested node was
        // traversed outward, and anything else arrived at it.
        let direction = if edge.from.key() == terminal.source.key() {
            Direction::Outgoing
        } else {
            Direction::Incoming
        };
        let label = relation_label(edge.kind, direction).as_str();
        if current != Some(label) {
            current = Some(label);
            let _ = writeln!(out, "## {label}");
        }
        let hop = edge.hop.get();
        let _ = writeln!(
            out,
            "- {}{}{}",
            relative.target(&edge.to),
            if hop > 1 { format!("  hop {hop}") } else { String::new() },
            confidence_suffix(edge.confidence)
        );
    }
    out
}

fn coverage_signal(coverage: Coverage) -> String {
    match coverage {
        Coverage::Complete => "complete".to_owned(),
        Coverage::Partial { searched, total } => format!("partial {}/{}", searched.0, total.0),
        Coverage::Degraded { reason } => {
            format!("degraded {}", common::degradation_slug(reason))
        }
        Coverage::Unavailable { reason } => format!("unavailable {}", unavailability_slug(reason)),
    }
}

// ── health ──────────────────────────────────────────────────────────────────────────────────────

fn health_markdown(health: &Health) -> String {
    let mut out = String::new();
    heading(&mut out, "health");
    for (capability, state) in health.rows() {
        let _ = writeln!(
            out,
            "{} {}  {}",
            capability_glyph(state),
            capability.label(),
            capability_slug(state)
        );
        if let crate::CapabilityState::Unreachable { detail } = state {
            let _ = writeln!(out, "  {detail}");
        }
    }
    out
}


#[cfg(test)]
mod tests {
    use compiler_ir::{EntityId, LinkKind, Visibility};
    use compiler_ir_vocabulary::{
        DeclarationFamilyId, DeclarationIdentity, EntityKind, VariantFingerprint,
    };
    use interface_core::{PackageEcosystem, PackageUrl};
    use interface_documents::{
        Census, Count, Direction, ExternalRef, ForeignOrigin, MemberRow, Name, Prose,
        RelationRole, RelationRow, Text, Token, TokenKind,
    };
    use interface_identity::{ContentKey, ExactAddress, SymbolPath};
    use interface_search::{
        Coverage, Degradation, Hit, Lane, LaneReport, LaneSet, QueryText, ResultLimit, Score,
        SearchRequest, SearchScope, Unavailability,
    };

    use crate::{AddRejection, LibraryEpoch, PackageCard, ShelfStatus};

    use super::*;

    fn context() -> RenderContext {
        RenderContext {
            now: Timestamp(1_000_000),
        }
    }

    fn coordinate(name: &str, version: &str) -> PackageCoordinate {
        match PackageCoordinate::new(PackageEcosystem::Cargo, name, version) {
            Ok(coordinate) => coordinate,
            Err(_) => unreachable!("fixture coordinates are well formed"),
        }
    }

    fn key(seed: u8) -> ContentKey {
        ContentKey::new(DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([seed; 16]),
            variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
        })
    }

    fn symbol(package: &PackageCoordinate, path: &str, kind: EntityKind, seed: u8) -> Symbol {
        let parsed = match SymbolPath::parse(path) {
            Ok(parsed) => parsed,
            Err(_) => unreachable!("fixture paths are well formed"),
        };
        let leaf = parsed
            .leaf()
            .map_or_else(|| package.name.as_str().to_owned(), |segment| {
                segment.name.as_str().to_owned()
            });
        Symbol {
            address: ExactAddress::mint(package.clone(), parsed, key(seed)),
            entity: EntityId(u32::from(seed)),
            name: Name::displayable(leaf.as_bytes()),
            kind,
            visibility: Visibility::Public,
        }
    }

    fn signature(text: &str) -> Signature {
        Signature::new(vec![Token {
            kind: TokenKind::Text,
            text: Text::new(text),
            target: None,
        }])
    }

    /// A page whose every field exercises one rendering rule: a same-package relation, a
    /// cross-package one, an external one, members of two kinds, and a source location.
    fn page() -> Page {
        let serde = coordinate("serde", "1.0.196");
        let other = coordinate("serde_json", "1.0.117");
        Page {
            symbol: symbol(
                &serde,
                "de::Deserializer[trait]::deserialize_map[fn]",
                EntityKind::Function,
                0x1c,
            ),
            language: Language::Rust,
            crumbs: Box::new([
                symbol(&serde, "de", EntityKind::Module, 0x40),
                symbol(&serde, "de::Deserializer[trait]", EntityKind::Trait, 0x41),
            ]),
            signature: signature(
                "fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>",
            ),
            prose: Prose::new(vec![Block::Paragraph(Box::new([Inline::Text(Text::new(
                "Hint that the `Deserialize` type is expecting a map of key-value pairs.",
            ))]))]),
            members: Box::new([MemberGroup {
                kind: EntityKind::Function,
                rows: Box::new([
                    MemberRow {
                        symbol: symbol(
                            &serde,
                            "de::Deserializer[trait]::deserialize_any[fn]",
                            EntityKind::Function,
                            0x2d,
                        ),
                        signature: signature(
                            "fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>",
                        ),
                        summary: None,
                    },
                    MemberRow {
                        symbol: symbol(
                            &serde,
                            "de::Deserializer[trait]::deserialize_seq[fn]",
                            EntityKind::Function,
                            0x3e,
                        ),
                        signature: signature("fn deserialize_seq<V>(self, visitor: V) -> Result<V, E|F>"),
                        summary: None,
                    },
                ]),
            }]),
            relations: Box::new([RelationGroup {
                role: RelationRole {
                    kind: LinkKind::Calls,
                    direction: Direction::Outgoing,
                },
                rows: Box::new([
                    RelationRow {
                        target: Target::Local(symbol(
                            &serde,
                            "de::Error[trait]::custom[fn]",
                            EntityKind::Function,
                            0x51,
                        )),
                        confidence: Confidence::Compiler,
                    },
                    RelationRow {
                        target: Target::Local(symbol(
                            &other,
                            "value::Value[enum]",
                            EntityKind::Enum,
                            0x62,
                        )),
                        confidence: Confidence::Heuristic,
                    },
                    RelationRow {
                        target: Target::External(ExternalRef {
                            display: Text::new("core::fmt::Display"),
                            path: Text::new("fmt::Display"),
                            origin: Some(ForeignOrigin {
                                ecosystem: Text::new("cargo"),
                                package: Some(Text::new("core")),
                            }),
                            kind: Some(EntityKind::Trait),
                        }),
                        confidence: Confidence::Imported,
                    },
                ]),
            }]),
            source: Some(SourceLocation {
                file: Text::new("src/de/mod.rs"),
                span: match interface_documents::ByteSpan::new(41_203, 41_876) {
                    Some(span) => span,
                    None => unreachable!("the fixture span is not inverted"),
                },
            }),
            attributes: Box::new([]),
        }
    }

    fn rendered_page() -> String {
        render(&Reply::Page(Ok(page())), &context())
    }

    #[test]
    fn a_page_leads_with_its_address_and_one_meta_line() {
        let text = rendered_page();
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some(
                "# cargo:serde@1.0.196::de::Deserializer[trait]::deserialize_map[fn]#1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c.1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d"
            )
        );
        assert_eq!(lines.next(), Some(""));
        assert_eq!(
            lines.next(),
            Some(
                "fn · public · 1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c.1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d · src/de/mod.rs:41203..41876"
            )
        );
    }

    #[test]
    fn the_trail_line_is_gone_because_the_heading_is_the_trail() {
        let text = rendered_page();
        assert!(!text.contains('↑'), "the trail line was reintroduced: {text}");
        assert!(
            !text.contains(" › de[mod] › "),
            "the trail line was reintroduced: {text}"
        );
    }

    #[test]
    fn the_coordinate_appears_exactly_once_and_only_in_the_heading() {
        let text = rendered_page();
        assert_eq!(
            text.matches("cargo:serde@1.0.196").count(),
            1,
            "the page's own coordinate must be written once: {text}"
        );
        assert!(
            text.contains("- cargo:serde_json › value::Value[enum]"),
            "another loaded package is named without repeating a version: {text}"
        );
        assert!(
            text.contains("- ⟨cargo core⟩ fmt::Display"),
            "an unloaded package reads as not-here: {text}"
        );
    }

    #[test]
    fn a_member_row_is_its_signature_and_never_a_name_repeated_before_it() {
        let text = rendered_page();
        assert!(
            text.contains(
                "- fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>  ~2d2d2d2d"
            ),
            "got: {text}"
        );
        assert!(
            !text.contains("- deserialize_any  fn deserialize_any"),
            "a member must not spell its name twice: {text}"
        );
        for line in text.lines().filter(|line| line.starts_with("- fn ")) {
            let name = line
                .split_once("- fn ")
                .and_then(|(_, rest)| rest.split_once('<').or_else(|| rest.split_once('(')))
                .map(|(name, _)| name)
                .unwrap_or_default();
            assert_eq!(
                line.matches(name).count(),
                1,
                "the member name is repeated inside its own row: {line}"
            );
        }
    }

    #[test]
    fn the_members_block_says_once_how_its_rows_are_addressed() {
        let text = rendered_page();
        assert_eq!(
            text.matches("members are addressable as").count(),
            1,
            "the footer belongs under the block, once: {text}"
        );
    }

    #[test]
    fn no_address_or_signature_is_ever_wrapped_in_a_table() {
        let text = rendered_page();
        for line in text.lines() {
            assert!(
                !line.trim_start().starts_with('|'),
                "a table row would corrupt the spelling it carries: {line}"
            );
        }
        assert!(
            text.contains("Result<V, E|F>"),
            "a union pipe inside a signature must survive verbatim: {text}"
        );
    }

    #[test]
    fn a_relation_prints_its_confidence_only_when_it_is_weaker_than_proved() {
        let text = rendered_page();
        assert!(text.contains("- de::Error[trait]::custom[fn]\n"), "got: {text}");
        assert!(text.contains("value::Value[enum]  heuristic"), "got: {text}");
        assert!(!text.contains("compiler\n"), "a proved edge says nothing: {text}");
    }

    fn terminal() -> SearchTerminal {
        let serde = coordinate("serde", "1.0.196");
        let request = SearchRequest {
            text: match QueryText::new("deserialize map") {
                Ok(text) => text,
                Err(_) => unreachable!("the fixture query is admissible"),
            },
            scope: SearchScope::default(),
            lanes: LaneSet::ALL,
            limit: ResultLimit::default(),
            cursor: None,
        };
        SearchTerminal {
            request,
            hits: Box::new([Hit {
                symbol: symbol(
                    &serde,
                    "de::Deserializer[trait]::deserialize_map[fn]",
                    EntityKind::Function,
                    0x1c,
                ),
                signature: Some(signature(
                    "fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>",
                )),
                summary: Some(Text::new("Hint that the type is expecting a map.")),
                lane: Lane::Exact,
                score: Score(1_000),
            }]),
            lanes: [
                LaneReport {
                    lane: Lane::Exact,
                    coverage: Coverage::Complete,
                    hits: Count(2),
                    elapsed: None,
                },
                LaneReport {
                    lane: Lane::Lexical,
                    coverage: Coverage::Partial {
                        searched: Count(3),
                        total: Count(7),
                    },
                    hits: Count(5),
                    elapsed: None,
                },
                LaneReport {
                    lane: Lane::Graph,
                    coverage: Coverage::Unavailable {
                        reason: Unavailability::NoIndex,
                    },
                    hits: Count(0),
                    elapsed: None,
                },
                LaneReport {
                    lane: Lane::Semantic,
                    coverage: Coverage::Unavailable {
                        reason: Unavailability::NoEmbedder,
                    },
                    hits: Count(0),
                    elapsed: None,
                },
            ],
            truncation: Truncation::Complete,
        }
    }

    #[test]
    fn a_search_states_every_lane_with_its_count_and_its_reason() {
        let text = render(&Reply::Searched(terminal()), &context());
        assert!(
            text.contains("~lanes exact✓2 names◐5 3/7 graph✗ no-index semantic✗ no-embedder"),
            "got: {text}"
        );
    }

    #[test]
    fn a_search_row_carries_no_raw_score() {
        let text = render(&Reply::Searched(terminal()), &context());
        assert!(
            text.contains(
                "ƒ cargo:serde@1.0.196::de::Deserializer[trait]::deserialize_map[fn]#1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c.1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d  ~1c1c1c1c  exact"
            ),
            "got: {text}"
        );
        assert!(!text.contains("1000"), "a lane's raw score is noise: {text}");
    }

    #[test]
    fn a_default_narrowing_is_stated_and_a_caller_chosen_one_is_not_invented() {
        let text = render(&Reply::Searched(terminal()), &context());
        assert!(
            text.contains("~scope kinds exclude field variant param"),
            "got: {text}"
        );
        let mut wide = terminal();
        wide.request.scope.kinds = interface_search::KindSet::ALL;
        let text = render(&Reply::Searched(wide), &context());
        assert!(
            !text.contains("~scope"),
            "an unnarrowed request says nothing: {text}"
        );
    }

    #[test]
    fn a_degraded_lane_names_its_degradation_rather_than_claiming_completeness() {
        let mut degraded = terminal();
        degraded.lanes[1].coverage = Coverage::Degraded {
            reason: Degradation::StaleProjection,
        };
        let text = render(&Reply::Searched(degraded), &context());
        assert!(text.contains("names◐5 stale-projection"), "got: {text}");
    }

    fn shelf(status: ShelfStatus) -> Shelf {
        Shelf {
            entries: Box::new([ShelfEntry {
                coordinate: coordinate("serde", "1.0.196"),
                status,
                requested_at: Timestamp(0),
                correlation: interface_core::CorrelationId(1),
            }]),
            epoch: LibraryEpoch::default(),
        }
    }

    fn census() -> Census {
        let mut census = Census::default();
        for _ in 0..412 {
            census.record(EntityKind::Function, true, true);
        }
        for _ in 0..88 {
            census.record(EntityKind::Record, true, true);
        }
        for _ in 0..21 {
            census.record(EntityKind::Trait, true, true);
        }
        census
    }

    #[test]
    fn an_empty_shelf_offers_the_call_that_would_fill_it() {
        let empty = Shelf {
            entries: Box::new([]),
            epoch: LibraryEpoch::default(),
        };
        let text = render(&Reply::Packages(Ok(empty)), &context());
        assert!(text.contains("(no packages)"), "got: {text}");
        assert!(
            text.contains("  → add {\"package\":\"pkg:cargo/serde@1.0.196\"}"),
            "got: {text}"
        );
    }

    #[test]
    fn a_compiling_row_draws_the_eight_step_journey_it_is_partway_through() {
        let text = render(
            &Reply::Packages(Ok(shelf(ShelfStatus::Compiling {
                phase: interface_core::PackageCompilePhase::Authority,
            }))),
            &context(),
        );
        assert!(
            text.contains("◐ cargo:serde@1.0.196  ●●●○○○○○ analyze"),
            "got: {text}"
        );
    }

    #[test]
    fn a_failed_row_keeps_the_diagnostic_the_compiler_retained() {
        let text = render(
            &Reply::Packages(Ok(shelf(ShelfStatus::Failed {
                cause: crate::ShelfFailure::Compiler {
                    summary: "cannot find macro `matches` in this scope".into(),
                },
            }))),
            &context(),
        );
        assert!(text.contains("✗ cargo:serde@1.0.196  compiler-rejected"), "got: {text}");
        assert!(
            text.contains("\n  cannot find macro `matches` in this scope\n"),
            "the retained diagnostic must sit under its row: {text}"
        );
    }

    #[test]
    fn a_ready_row_counts_its_declarations_and_dates_itself() {
        let card = PackageCard {
            coordinate: coordinate("serde", "1.0.196"),
            profile: compiler_vocabulary::LanguageProfile::Rust(
                compiler_vocabulary::RustEdition::Rust2021,
            ),
            generation: heart_identity::GenerationId::default(),
            image: interface_core::SemanticImageAuthority {
                identity: Default::default(),
                byte_len: 0,
            },
            census: census(),
            published_at: Timestamp(1_000_000 - 7_200),
        };
        let text = render(
            &Reply::Packages(Ok(shelf(ShelfStatus::Ready { card }))),
            &context(),
        );
        assert!(
            text.contains("✓ cargo:serde@1.0.196  fn 412 · struct 88 · trait 21  2h ago"),
            "got: {text}"
        );
    }

    #[test]
    fn a_refused_add_hands_back_the_exact_spelling_that_was_refused() {
        let url = match PackageUrl::try_from("pkg:cargo/serde@1.0.196".to_owned()) {
            Ok(url) => url,
            Err(_) => unreachable!("the fixture url is well formed"),
        };
        let text = render(
            &Reply::Added(AddOutcome::Rejected(RejectedAdd {
                url,
                rejection: AddRejection::CompilerDetached,
            })),
            &context(),
        );
        assert!(
            text.contains("✗ compiler-detached pkg:cargo/serde@1.0.196"),
            "got: {text}"
        );
        assert!(text.contains("  → health {}"), "got: {text}");
    }
}
