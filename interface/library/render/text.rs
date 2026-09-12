//! Defines text behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the text invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One [`Reply`] rendered for a terminal: a pure function of the reply, a width, a palette, and a clock.
//!
//! The renderer never asks whether it is attached to a terminal. The caller decides that once and
//! hands in [`Palette::plain`] or [`Palette::ansi`], so "no escape codes when the output is piped"
//! holds by construction rather than by discipline, and the GPUI surface can reuse this function
//! with a plain palette to obtain the exact strings the CLI prints.
//!
//! Every rule this renderer shares with the Markdown surface — how a target is spelled relative to
//! a page, how a fault reads, which slug names which failure — comes from
//! [`crate::render::common`] rather than being spelled again here. What is local to this module is
//! typography: widths, columns, ellipses, and which parts of a line are dim.

use core::fmt::Write as _;

use compiler_ir_vocabulary::EntityKind;
use interface_documents::{
    Block, Census, Count, Direction, Inline, MemberGroup, MemberRow, MissingPool, Outline,
    OutlineNode, Page, PageTruncation, PageVisitor, ProjectionError, RelationGroup, Signature,
    SourceLocation,
    Symbol, walk_page,
};
use interface_identity::{
    AddressParseError, CoordinateParseError, KeyParseError, KindTag, PackageCoordinate,
    PathParseError,
};
use interface_search::{
    GraphTerminal, Hit, Lane, LaneReport, SearchTerminal, Truncation, relation_label,
};

use crate::{
    AddFailure, AddOutcome, AddRejection, Capability, CapabilityState, CompilePhaseProgress,
    ExploreCoverage, ExploreError, Health, IndexSearchPage, PackageProfile, PackageVersionRow,
    PackageVersionRows, PageError, RejectedAdd, RemoveOutcome, Reply, Resolution, ResolveError,
    Shelf, ShelfEntry, ShelfError, ShelfFailure, ShelfStatus, Timestamp,
    render::common::{
        Affordance, Affordances, EXAMPLE_PACKAGE_URL, Fault, RelativeAddress, RenderContext,
        add_affordance, capability_glyph, capability_slug, explore_error_detail,
        explore_error_slug, explore_unavailable_slug, lane_signal, package_url_slug, phase_dots,
        rejection_slug, shelf_failure_slug, visibility_label, write_fault,
    },
};

/// Rendered terminal width in character cells.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Width(u16);

impl Width {
    /// Width assumed when no terminal reports one.
    pub const DEFAULT: Self = Self(100);
    /// Widest line of flowing prose, however wide the terminal is; long measures do not read.
    pub const PROSE_MAXIMUM: u16 = 88;
    /// Narrowest width this renderer will lay out for.
    pub const MINIMUM: u16 = 40;

    /// Admits a width, refusing to go below a legible floor.
    #[must_use]
    pub const fn new(columns: u16) -> Self {
        if columns < Self::MINIMUM {
            Self(Self::MINIMUM)
        } else {
            Self(columns)
        }
    }

    /// Full width available for a row.
    #[must_use]
    pub const fn columns(self) -> usize {
        self.0 as usize
    }

    /// Width flowing prose wraps at: never wider than [`Width::PROSE_MAXIMUM`].
    #[must_use]
    pub const fn prose(self) -> usize {
        if self.0 < Self::PROSE_MAXIMUM {
            self.0 as usize
        } else {
            Self::PROSE_MAXIMUM as usize
        }
    }
}

impl Default for Width {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Whether rendered text carries ANSI attributes.
///
/// Two constructors and no setter: a caller that decided "this is a pipe" cannot later leak an
/// escape code, because the plain palette has no way to emit one.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Palette {
    coloured: bool,
}

impl Palette {
    /// A palette that emits no escape codes at all.
    #[must_use]
    pub const fn plain() -> Self {
        Self { coloured: false }
    }

    /// A palette that emits ANSI SGR attributes.
    #[must_use]
    pub const fn ansi() -> Self {
        Self { coloured: true }
    }

    /// Whether this palette emits escape codes.
    #[must_use]
    pub const fn is_coloured(self) -> bool {
        self.coloured
    }

    /// Secondary text: counts, lane tags, group headers, hints.
    #[must_use]
    pub fn dim(self, text: &str) -> String {
        self.attribute("2", text)
    }

    /// Primary text: the identity a reader came for.
    #[must_use]
    pub fn strong(self, text: &str) -> String {
        self.attribute("1", text)
    }

    /// A failure marker.
    #[must_use]
    pub fn fault(self, text: &str) -> String {
        self.attribute("31", text)
    }

    /// A success marker.
    #[must_use]
    pub fn good(self, text: &str) -> String {
        self.attribute("32", text)
    }

    /// A kind glyph or relation label.
    #[must_use]
    pub fn accent(self, text: &str) -> String {
        self.attribute("36", text)
    }

    fn attribute(self, code: &str, text: &str) -> String {
        if self.coloured && !text.is_empty() {
            let mut out = String::with_capacity(text.len() + 9);
            out.push_str("\u{1b}[");
            out.push_str(code);
            out.push('m');
            out.push_str(text);
            out.push_str("\u{1b}[0m");
            out
        } else {
            text.to_owned()
        }
    }
}

/// Whether the process that produced this reply brought a compiler.
///
/// A detached process has one affordance no attached process has, and it is the honest answer to
/// every fault it cannot otherwise resolve: run again without detaching.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Attachment {
    /// The library was opened with a compiler.
    #[default]
    Attached,
    /// The library was opened read-only.
    Detached,
}

/// Everything the terminal renderer needs that a [`Reply`] does not carry.
///
/// `neighbours` exists so an unknown-package fault can name what *is* on the shelf instead of
/// telling a reader only what is absent. `subject` exists because [`RemoveOutcome`] and
/// [`Resolution`] are terminals that do not retain the operand they answered, and a fault without
/// its operand is chrome.
#[derive(Clone, Copy, Debug)]
pub struct TextOptions<'context> {
    /// Width the caller measured.
    pub width: Width,
    /// Colour decision the caller already made.
    pub palette: Palette,
    /// Clock and anything else shared with the Markdown surface.
    pub context: RenderContext,
    /// Whether this process brought a compiler.
    pub attachment: Attachment,
    /// Coordinates currently on the shelf, for "nearest" evidence.
    pub neighbours: &'context [PackageCoordinate],
    /// Exact operand the caller supplied, for replies whose terminal does not retain one.
    pub subject: Option<&'context str>,
}

impl Default for TextOptions<'_> {
    fn default() -> Self {
        Self {
            width: Width::DEFAULT,
            palette: Palette::plain(),
            context: RenderContext { now: Timestamp(0) },
            attachment: Attachment::Attached,
            neighbours: &[],
            subject: None,
        }
    }
}

/// How a terminal spells the one call that could make progress past a fault.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerminalAffordances {
    attachment: Attachment,
}

impl TerminalAffordances {
    /// Spells affordances for a process with this attachment.
    #[must_use]
    pub const fn new(attachment: Attachment) -> Self {
        Self { attachment }
    }
}

impl Affordances for TerminalAffordances {
    /// A detached process always has one more move than the affordance itself knows about, so
    /// [`Affordance::None`] is answered with it rather than with a shrug.
    fn spell(&self, affordance: &Affordance) -> Option<String> {
        match affordance {
            Affordance::None => match self.attachment {
                Attachment::Attached => None,
                Attachment::Detached => Some("run without --detached".to_owned()),
            },
            Affordance::Packages => Some("nudox packages".to_owned()),
            Affordance::Health => Some("nudox health".to_owned()),
            Affordance::Add { package } => Some(format!("nudox add {package}")),
            Affordance::Resolve { text } => Some(format!("nudox resolve \"{text}\"")),
            Affordance::Search { query } => Some(format!("nudox search \"{query}\"")),
            Affordance::Show { address } => Some(format!("nudox show {address}")),
        }
    }
}

/// Renders one reply for a terminal.
#[must_use]
pub fn render(reply: &Reply, options: &TextOptions<'_>) -> String {
    match reply {
        Reply::Packages(Ok(shelf)) => shelf_text(shelf, options),
        Reply::Packages(Err(error)) => fault(&shelf_fault(error), options),
        Reply::Added(outcome) => added_text(outcome, options),
        Reply::Removed(outcome) => removed_text(outcome, options),
        Reply::Page(Ok(page)) => page_text(page, options),
        Reply::Page(Err(error)) | Reply::Outline(Err(error)) | Reply::Graphed(Err(error)) => {
            page_error_text(error, options)
        }
        Reply::Outline(Ok(outline)) => outline_text(outline, options),
        Reply::Resolved(Ok(resolution)) => resolution_text(resolution, options),
        Reply::Resolved(Err(error)) => fault(&resolve_fault(error, options), options),
        Reply::Searched(terminal) => search_text(terminal, options),
        Reply::Graphed(Ok(terminal)) => graph_text(terminal, options),
        Reply::Health(health) => health_text(health, options),
        Reply::IndexSearched(Ok(page)) => index_search_text(page, options),
        Reply::Versions(Ok(rows)) => versions_text(rows, options),
        Reply::Profiled(Ok(profile)) => profile_text(profile, options),
        Reply::IndexSearched(Err(error))
        | Reply::Versions(Err(error))
        | Reply::Profiled(Err(error)) => fault(&explore_fault(error, options), options),
    }
}

/// The typed failure inside one reply, when the reply carries one.
///
/// Every surface that has to name a failure — the terminal, the JSON envelope, a GUI panel — reads
/// it from here, so the slug a person sees and the slug a program branches on cannot drift apart.
#[must_use]
pub fn fault_of(reply: &Reply, options: &TextOptions<'_>) -> Option<Fault> {
    match reply {
        Reply::Packages(Err(error)) => Some(shelf_fault(error)),
        Reply::Added(AddOutcome::Rejected(rejected)) => Some(rejection_fault(rejected)),
        Reply::Added(AddOutcome::Failed(failure)) => Some(add_failure_fault(failure, options)),
        Reply::Removed(RemoveOutcome::Absent) => Some(
            Fault::new("not-on-shelf", options.subject.unwrap_or_default(), Affordance::Packages)
                .detailed("the shelf has no row for this package"),
        ),
        Reply::Removed(RemoveOutcome::Busy) => Some(
            Fault::new("compiler-busy", options.subject.unwrap_or_default(), Affordance::Packages)
                .detailed("a live process is compiling this package, so the row was left alone"),
        ),
        Reply::Page(Err(error)) | Reply::Outline(Err(error)) | Reply::Graphed(Err(error)) => {
            Some(page_fault(error, options))
        }
        Reply::Resolved(Err(error)) => Some(resolve_fault(error, options)),
        Reply::Resolved(Ok(Resolution::Ambiguous(candidates))) => {
            Some(ambiguity_fault(candidates.len(), options))
        }
        Reply::Resolved(Ok(Resolution::Unknown { package })) => {
            Some(unknown_path_fault(package, options))
        }
        Reply::Searched(terminal) if every_requested_lane_unavailable(terminal) => Some(
            Fault::new(
                "no-lane-ran",
                terminal.request.text.as_str().to_owned(),
                Affordance::Health,
            )
            .detailed("every requested lane refused to run, so zero rows means nothing"),
        ),
        Reply::IndexSearched(Err(error))
        | Reply::Versions(Err(error))
        | Reply::Profiled(Err(error)) => Some(explore_fault(error, options)),
        Reply::IndexSearched(Ok(page)) => match page.coverage {
            ExploreCoverage::Unavailable { reason } => Some(
                Fault::new(
                    explore_unavailable_slug(reason),
                    options.subject.unwrap_or_default(),
                    Affordance::Health,
                )
                .detailed("the durable index could not be searched, so zero rows means nothing"),
            ),
            _ => None,
        },
        _ => None,
    }
}

/// Renders one fault in the grammar every surface shares.
///
/// The grammar itself is [`common::write_fault`]; nothing is added here, so a reader who learns
/// `✗ slug operand` in a terminal reads the identical three lines in an agent transcript.
#[must_use]
pub fn fault(value: &Fault, options: &TextOptions<'_>) -> String {
    let mut out = String::new();
    write_fault(&mut out, value, &TerminalAffordances::new(options.attachment));
    out
}

fn fault_with_evidence(value: &Fault, evidence: &[String], options: &TextOptions<'_>) -> String {
    let mut out = fault(value, options);
    for row in evidence {
        line(&mut out, &options.palette.dim(&format!("  {row}")));
    }
    out
}

fn line(out: &mut String, text: &str) {
    out.push_str(text);
    out.push('\n');
}

// ---------------------------------------------------------------- shelf

fn shelf_text(shelf: &Shelf, options: &TextOptions<'_>) -> String {
    if shelf.entries.is_empty() {
        return fault(
            &Fault::new(
                "shelf-empty",
                "",
                Affordance::Add {
                    package: EXAMPLE_PACKAGE_URL.to_owned(),
                },
            )
            .detailed("no package has been added to this library yet"),
            options,
        );
    }
    let palette = options.palette;
    let ready = shelf.ready().count();
    let total = shelf.entries.len();
    let noun = if total == 1 { "package" } else { "packages" };
    let mut out = String::new();
    line(&mut out, &palette.dim(&format!("{total} {noun} \u{b7} {ready} ready")));
    for entry in &shelf.entries {
        shelf_row(&mut out, entry, options);
    }
    out
}

fn shelf_row(out: &mut String, entry: &ShelfEntry, options: &TextOptions<'_>) {
    let palette = options.palette;
    let coordinate = entry.coordinate.to_string();
    match &entry.status {
        ShelfStatus::Ready { card } => line(
            out,
            &format!(
                "{} {coordinate}  {}  {}",
                palette.good("\u{2713}"),
                palette.dim(&counts_line(&card.census)),
                palette.dim(&compact_age(options.context.now, card.published_at))
            ),
        ),
        ShelfStatus::Compiling { phase } => line(
            out,
            &format!(
                "{} {coordinate}  {} {}",
                palette.accent("\u{25d0}"),
                phase_dots(*phase),
                palette.dim(CompilePhaseProgress::of(*phase).label())
            ),
        ),
        ShelfStatus::Requested => line(
            out,
            &format!("{} {coordinate}  {}", palette.dim("\u{b7}"), palette.dim("requested")),
        ),
        ShelfStatus::Failed { cause } => {
            line(
                out,
                &format!(
                    "{} {coordinate}  {}",
                    palette.fault("\u{2717}"),
                    palette.dim(shelf_failure_slug(cause))
                ),
            );
            if let Some(detail) = shelf_failure_detail(cause) {
                line(out, &format!("  {}", palette.dim(detail)));
            }
        }
    }
}

/// Declaration counts as a shelf row spells them: totals, not a kind histogram.
///
/// [`common::census_line`] spells the histogram the Markdown surface shows on a package card; a
/// terminal shelf is a column of rows, and three totals fit where a histogram does not.
#[must_use]
pub fn counts_line(census: &Census) -> String {
    format!(
        "{} decls \u{b7} {} pub \u{b7} {} doc",
        census.entities.0, census.public.0, census.documented.0
    )
}

/// Age as a column entry: `2h`, not `2h ago`.
///
/// [`common::relative_age`] spells the sentence form prose needs; a column of ages reads better
/// without the word repeated on every row.
#[must_use]
pub fn compact_age(now: Timestamp, then: Timestamp) -> String {
    let seconds = now.0.saturating_sub(then.0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

const fn shelf_failure_detail(cause: &ShelfFailure) -> Option<&str> {
    match cause {
        ShelfFailure::PackageNotFound => {
            Some("the package directory is not present beneath the configured ecosystem root")
        }
        ShelfFailure::EcosystemRootUnavailable => {
            Some("no source root is configured for this ecosystem")
        }
        ShelfFailure::Compiler { summary } => Some(summary),
        ShelfFailure::Publication => Some("durable publication failed"),
        ShelfFailure::Cancelled | ShelfFailure::Orphaned => None,
    }
}

fn shelf_fault(error: &ShelfError) -> Fault {
    match error {
        ShelfError::Store { detail } => {
            Fault::new("shelf-unreadable", "", Affordance::Health).detailed(detail.as_ref())
        }
        ShelfError::Capacity { maximum } => Fault::new("shelf-full", "", Affordance::Packages)
            .detailed(format!("this library retains at most {maximum} packages")),
        ShelfError::Epoch(_) => Fault::new("epoch-unreadable", "", Affordance::Health)
            .detailed("the library epoch file could not be read"),
    }
}

// ---------------------------------------------------------------- add and remove

fn added_text(outcome: &AddOutcome, options: &TextOptions<'_>) -> String {
    let palette = options.palette;
    match outcome {
        AddOutcome::Ready { card } => {
            let mut out = String::new();
            line(
                &mut out,
                &format!(
                    "{} {}  {}",
                    palette.good("\u{2713}"),
                    card.coordinate,
                    palette.dim(&counts_line(&card.census))
                ),
            );
            out
        }
        AddOutcome::Rejected(rejected) => fault(&rejection_fault(rejected), options),
        AddOutcome::Failed(failure) => fault(&add_failure_fault(failure, options), options),
    }
}

fn add_failure_fault(failure: &AddFailure, options: &TextOptions<'_>) -> Fault {
    let value = Fault::new(
        shelf_failure_slug(&failure.cause),
        options.subject.unwrap_or_default(),
        Affordance::Packages,
    );
    match shelf_failure_detail(&failure.cause) {
        Some(detail) => value.detailed(detail),
        None => value,
    }
}

fn rejection_fault(rejected: &RejectedAdd) -> Fault {
    let operand: &str = rejected.url.as_ref();
    let slug = rejection_slug(&rejected.rejection);
    match &rejected.rejection {
        AddRejection::PackageUrl { cause } => Fault::new(slug, operand, Affordance::None)
            .detailed(format!("the {} is not spelled correctly", package_url_slug(*cause))),
        AddRejection::CompilerDetached => Fault::new(slug, operand, Affordance::None)
            .detailed("this process opened the library without a compiler"),
        AddRejection::Busy { active } => {
            let detail = match active {
                Some(coordinate) => format!("another process is compiling {coordinate}"),
                None => "another process holds the compile lock".to_owned(),
            };
            Fault::new(slug, operand, Affordance::Packages).detailed(detail)
        }
        AddRejection::AlreadyReady => Fault::new(slug, operand, Affordance::Packages)
            .detailed("this package is already published and readable"),
        AddRejection::ShelfFull { maximum } => Fault::new(slug, operand, Affordance::Packages)
            .detailed(format!("this library retains at most {maximum} packages")),
    }
}

fn removed_text(outcome: &RemoveOutcome, options: &TextOptions<'_>) -> String {
    let subject = options.subject.unwrap_or_default();
    match outcome {
        RemoveOutcome::Removed => {
            let mut out = String::new();
            line(&mut out, &format!("{} removed {subject}", options.palette.good("\u{2713}")));
            out
        }
        RemoveOutcome::Absent => fault(
            &Fault::new("not-on-shelf", subject, Affordance::Packages)
                .detailed("the shelf has no row for this package"),
            options,
        ),
        RemoveOutcome::Busy => fault(
            &Fault::new("compiler-busy", subject, Affordance::Packages)
                .detailed("a live process is compiling this package, so the row was left alone"),
            options,
        ),
    }
}

// ---------------------------------------------------------------- page

struct PageText<'render> {
    page: &'render Page,
    options: &'render TextOptions<'render>,
    out: String,
    members_seen: bool,
}

impl PageVisitor for PageText<'_> {
    fn header(&mut self, symbol: &Symbol) {
        let palette = self.options.palette;
        line(&mut self.out, &palette.strong(&symbol.address.to_string()));
        line(&mut self.out, &palette.dim(&meta_line(symbol, self.page.source.as_ref())));
    }

    /// The address line above already spells the whole trail. A second `de \u{203a} Deserializer`
    /// line would repeat it in a spelling a reader cannot copy back.
    fn trail(&mut self, _crumbs: &[Symbol]) {}

    /// A signature on a page is never truncated: it is the thing the reader came for.
    fn signature(&mut self, signature: &Signature) {
        self.out.push('\n');
        line(&mut self.out, signature.plain().trim_end());
    }

    fn prose_block(&mut self, block: &Block) {
        self.out.push('\n');
        match block {
            Block::Paragraph(inlines) => {
                for row in wrap(&paragraph_text(inlines), self.options.width.prose()) {
                    line(&mut self.out, &row);
                }
            }
            Block::Code(text) => {
                for row in text.as_str().lines() {
                    line(&mut self.out, &format!("  {row}"));
                }
            }
        }
    }

    fn members(&mut self, group: &MemberGroup) {
        self.members_seen = true;
        self.out.push('\n');
        member_group(&mut self.out, group, self.options);
    }

    fn relations(&mut self, group: &RelationGroup) {
        self.out.push('\n');
        relation_group(&mut self.out, group, self.page, self.options);
    }

    /// The source location is folded into the meta line, where a reader looks for coordinates.
    fn source(&mut self, _location: &SourceLocation) {}
}

fn page_text(page: &Page, options: &TextOptions<'_>) -> String {
    let mut visitor = PageText {
        page,
        options,
        out: String::new(),
        members_seen: false,
    };
    walk_page(page, &mut visitor);
    if visitor.members_seen {
        visitor.out.push('\n');
        line(
            &mut visitor.out,
            &options.palette.dim(&format!(
                "members are addressable as {}::{{name}}",
                page.symbol.address
            )),
        );
    }
    if let Some(note) = truncation_note(page.truncation) {
        visitor.out.push('\n');
        line(&mut visitor.out, &options.palette.dim(&note));
    }
    visitor.out
}

/// A page that shows a prefix says so. A renderer that silently drops the tail of a member list
/// tells the reader a declaration is smaller than it is, which is a lie the projector never told.
fn truncation_note(truncation: PageTruncation) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(count) = truncation.members {
        parts.push(format!("{} more members", count.0));
    }
    if let Some(count) = truncation.relations {
        parts.push(format!("{} more relations", count.0));
    }
    if let Some(count) = truncation.signature_tokens {
        parts.push(format!("{} more signature tokens", count.0));
    }
    (!parts.is_empty())
        .then(|| format!("\u{2026} {} the projection did not admit", parts.join(" \u{b7} ")))
}

fn meta_line(symbol: &Symbol, source: Option<&SourceLocation>) -> String {
    let mut parts = vec![format!(
        "{} {}",
        KindTag::glyph(symbol.kind),
        KindTag::of(symbol.kind).as_str()
    )];
    if let Some(label) = visibility_label(symbol.visibility) {
        parts.push(label.to_owned());
    }
    parts.push(symbol.key().abbreviation().to_string());
    if let Some(location) = source {
        parts.push(format!(
            "{}:{}..{}",
            location.file, location.span.start.0, location.span.end.0
        ));
    }
    parts.join(" \u{b7} ")
}

fn member_group(out: &mut String, group: &MemberGroup, options: &TextOptions<'_>) {
    line(
        out,
        &options
            .palette
            .dim(&format!("{} ({})", KindTag::of(group.kind).as_str(), group.rows.len())),
    );
    for row in &group.rows {
        member_row(out, row, group.kind, options);
    }
}

/// One member is one line. The signature preview already contains the member's name, so printing
/// the name and then the signature would say it twice; a member with no retained signature falls
/// back to its glyph and name, which is the only case where the name is printed alone.
fn member_row(out: &mut String, row: &MemberRow, kind: EntityKind, options: &TextOptions<'_>) {
    let palette = options.palette;
    let preview = if row.signature.is_empty() {
        format!("{} {}", KindTag::glyph(kind), row.symbol.name)
    } else {
        collapse(&row.signature.plain())
    };
    let preview = ellipsize(&preview, options.width.columns().saturating_sub(2));
    let Some(summary) = &row.summary else {
        line(out, &format!("  {preview}"));
        return;
    };
    let summary = summary.as_str().trim();
    if 2 + width_of(&preview) + 2 + width_of(summary) <= options.width.columns() {
        line(out, &format!("  {preview}  {}", palette.dim(summary)));
    } else {
        line(out, &format!("  {preview}"));
        let budget = options.width.prose().saturating_sub(4);
        line(out, &format!("    {}", palette.dim(&ellipsize(summary, budget))));
    }
}

fn relation_group(out: &mut String, group: &RelationGroup, page: &Page, options: &TextOptions<'_>) {
    let palette = options.palette;
    let label = relation_label(group.role.kind, group.role.direction);
    line(out, &palette.dim(&format!("{} ({})", label.as_str(), group.rows.len())));
    let relative = RelativeAddress::to(page.symbol.package());
    for row in &group.rows {
        line(out, &format!("  {}", relative.target(&row.target)));
    }
}

// ---------------------------------------------------------------- outline

fn outline_text(outline: &Outline, options: &TextOptions<'_>) -> String {
    let palette = options.palette;
    let mut out = String::new();
    line(&mut out, &palette.strong(&outline.package.to_string()));
    line(&mut out, &palette.dim(&counts_line(&outline.census)));
    if outline.roots.is_empty() {
        line(&mut out, &palette.dim("no declarations were retained for this package"));
        return out;
    }
    for (node, depth) in outline.walk() {
        outline_row(&mut out, node, depth, options);
    }
    out
}

fn outline_row(out: &mut String, node: &OutlineNode, depth: usize, options: &TextOptions<'_>) {
    let palette = options.palette;
    let head = format!(
        "{}{} {}",
        "  ".repeat(depth),
        KindTag::glyph(node.symbol.kind),
        node.symbol.name
    );
    let budget = options.width.columns().saturating_sub(width_of(&head) + 2);
    match &node.summary {
        Some(summary) if budget >= 8 => line(
            out,
            &format!("{head}  {}", palette.dim(&ellipsize(summary.as_str().trim(), budget))),
        ),
        _ => line(out, &head),
    }
}

// ---------------------------------------------------------------- resolve

fn resolution_text(resolution: &Resolution, options: &TextOptions<'_>) -> String {
    let palette = options.palette;
    match resolution {
        Resolution::Exact(symbol) => {
            let mut out = String::new();
            line(&mut out, &palette.strong(&symbol.address.to_string()));
            line(&mut out, &palette.dim(&meta_line(symbol, None)));
            out
        }
        Resolution::Ambiguous(candidates) => fault_with_evidence(
            &ambiguity_fault(candidates.len(), options),
            &candidate_lines(candidates),
            options,
        ),
        Resolution::Unknown { package } => fault(&unknown_path_fault(package, options), options),
    }
}

fn unknown_path_fault(package: &PackageCoordinate, options: &TextOptions<'_>) -> Fault {
    let subject = options.subject.unwrap_or_default();
    Fault::new(
        "path-unknown",
        subject,
        Affordance::Search {
            query: subject.to_owned(),
        },
    )
    .detailed(format!(
        "{package} is loaded but retains no declaration at this path"
    ))
}

fn ambiguity_fault(candidates: usize, options: &TextOptions<'_>) -> Fault {
    Fault::new(
        "ambiguous",
        options.subject.unwrap_or_default(),
        Affordance::Show {
            address: "<candidate>".to_owned(),
        },
    )
    .detailed(format!("{candidates} declarations share this spelling"))
}

fn candidate_lines(candidates: &[Symbol]) -> Vec<String> {
    candidates
        .iter()
        .enumerate()
        .map(|(index, symbol)| format!("{:>2}. {}", index + 1, symbol.address))
        .collect()
}

// ---------------------------------------------------------------- search

fn search_text(terminal: &SearchTerminal, options: &TextOptions<'_>) -> String {
    let palette = options.palette;
    let mut out = String::new();
    let home = single_scope(terminal);
    for (index, hit) in terminal.hits.iter().enumerate() {
        search_row(&mut out, index, hit, home.as_ref(), options);
    }
    if terminal.hits.is_empty() {
        line(&mut out, &palette.dim("no rows"));
    }
    line(&mut out, &palette.dim(&coverage_line(&terminal.lanes)));
    if let Truncation::Truncated { next } = terminal.truncation {
        line(
            &mut out,
            &palette.dim(&format!(
                "  \u{2192} nudox search \"{}\" --cursor {}",
                terminal.request.text.as_str(),
                next.0
            )),
        );
    }
    out
}

fn single_scope(terminal: &SearchTerminal) -> Option<PackageCoordinate> {
    match &terminal.request.scope.packages {
        Some(packages) if packages.len() == 1 => packages.first().cloned(),
        _ => None,
    }
}

/// One hit is a numbered row with the address never truncated, then the signature and summary
/// beneath it. The lane is a dim tag; the lane's raw score never reaches a human, because a number
/// only one lane knows how to compare is not a fact a reader can use.
fn search_row(
    out: &mut String,
    index: usize,
    hit: &Hit,
    home: Option<&PackageCoordinate>,
    options: &TextOptions<'_>,
) {
    let palette = options.palette;
    let address = match home {
        Some(home) => RelativeAddress::to(home).symbol(&hit.symbol),
        None => hit.symbol.address.to_string(),
    };
    line(
        out,
        &format!(
            "{:>2} {} {address}  {}",
            index + 1,
            KindTag::glyph(hit.symbol.kind),
            palette.dim(hit.lane.label())
        ),
    );
    if let Some(signature) = &hit.signature {
        let budget = options.width.columns().saturating_sub(3);
        line(out, &format!("   {}", ellipsize(&collapse(&signature.plain()), budget)));
    }
    if let Some(summary) = &hit.summary {
        let budget = options.width.prose().saturating_sub(3);
        line(
            out,
            &format!("   {}", palette.dim(&ellipsize(summary.as_str().trim(), budget))),
        );
    }
}

/// The one-line coverage signal, in the shared `~lanes` vocabulary.
#[must_use]
pub fn coverage_line(lanes: &[LaneReport; 4]) -> String {
    let mut out = String::from("~lanes");
    for report in lanes {
        out.push(' ');
        out.push_str(&lane_signal(report));
    }
    out
}

/// Whether every lane the caller asked for refused to run.
///
/// This is the difference between "nothing matched" and "nothing looked", and it is the only thing
/// that makes an otherwise successful search a failure worth a non-zero exit.
#[must_use]
pub fn every_requested_lane_unavailable(terminal: &SearchTerminal) -> bool {
    Lane::ALL
        .into_iter()
        .filter(|lane| terminal.request.lanes.contains(*lane))
        .all(|lane| {
            terminal
                .lanes
                .iter()
                .find(|report| report.lane == lane)
                .is_none_or(|report| !report.coverage.ran())
        })
}

// ---------------------------------------------------------------- explore

/// The one-line coverage signal for an index search, in the shared `~` vocabulary.
#[must_use]
pub fn index_coverage_signal(page: &IndexSearchPage) -> String {
    let hits = page.hits.len();
    match page.coverage {
        ExploreCoverage::Complete => format!("\u{2713}{hits}"),
        ExploreCoverage::Partial { searched, total } => {
            format!("\u{25d0}{hits} {searched}/{total}")
        }
        ExploreCoverage::Unavailable { reason } => {
            format!("\u{2717} {}", explore_unavailable_slug(reason))
        }
    }
}

fn index_search_text(page: &IndexSearchPage, options: &TextOptions<'_>) -> String {
    let palette = options.palette;
    let mut out = String::new();
    if page.hits.is_empty() {
        line(&mut out, &palette.dim("no rows"));
    }
    for (index, hit) in page.hits.iter().enumerate() {
        let _ = writeln!(out, "{:>2} {}", index + 1, hit.matched);
    }
    line(&mut out, &palette.dim(&format!("~index {}", index_coverage_signal(page))));
    out
}

fn versions_text(rows: &PackageVersionRows, options: &TextOptions<'_>) -> String {
    let mut out = String::new();
    for (index, row) in rows.rows.iter().enumerate() {
        line(&mut out, &version_row(index, row, options.palette));
    }
    out
}

fn profile_text(profile: &PackageProfile, options: &TextOptions<'_>) -> String {
    let palette = options.palette;
    let mut out = String::new();
    match &profile.latest {
        Some(latest) => line(
            &mut out,
            &format!(
                "{} {}  {}",
                palette.strong("latest"),
                latest.version.as_str(),
                latest.checksum.abbreviation()
            ),
        ),
        None => line(&mut out, &palette.dim("no versions")),
    }
    for (index, row) in profile.versions.rows.iter().enumerate() {
        line(&mut out, &version_row(index, row, palette));
    }
    out
}

fn version_row(index: usize, row: &PackageVersionRow, palette: Palette) -> String {
    let mut row_text = format!(
        "{:>2} {}  {}  cycle {}",
        index + 1,
        row.version.as_str(),
        row.checksum.abbreviation(),
        row.cycle.get()
    );
    if !row.active.is_active() {
        row_text.push_str("  ");
        row_text.push_str(&palette.dim("yanked"));
    }
    row_text
}

fn explore_fault(error: &ExploreError, options: &TextOptions<'_>) -> Fault {
    match error {
        ExploreError::QueryTooLong { .. } => Fault::new(
            explore_error_slug(error),
            options.subject.unwrap_or_default(),
            Affordance::None,
        )
        .detailed(explore_error_detail(error)),
        ExploreError::IndexStore { .. }
        | ExploreError::CatalogAbsent
        | ExploreError::Catalog { .. } => {
            Fault::new(explore_error_slug(error), "", Affordance::Health)
                .detailed(explore_error_detail(error))
        }
        ExploreError::NotFound { package } => Fault::new(
            explore_error_slug(error),
            package.as_str(),
            Affordance::Search {
                query: package.as_str().to_owned(),
            },
        )
        .detailed(explore_error_detail(error)),
        ExploreError::Ambiguous { package, .. } => Fault::new(
            explore_error_slug(error),
            package.as_str(),
            Affordance::Health,
        )
        .detailed(explore_error_detail(error)),
    }
}

// ---------------------------------------------------------------- graph

fn graph_text(terminal: &GraphTerminal, options: &TextOptions<'_>) -> String {
    let palette = options.palette;
    let mut out = String::new();
    line(&mut out, &palette.strong(&terminal.source.address.to_string()));
    let relative = RelativeAddress::to(terminal.source.package());
    if terminal.edges.is_empty() {
        line(&mut out, &palette.dim("no relations"));
    }
    for edge in &terminal.edges {
        let direction = if edge.from.key() == terminal.source.key() {
            Direction::Outgoing
        } else {
            Direction::Incoming
        };
        line(
            &mut out,
            &format!(
                "  {} {}",
                palette.dim(relation_label(edge.kind, direction).as_str()),
                relative.target(&edge.to)
            ),
        );
    }
    let report = LaneReport {
        lane: Lane::Graph,
        coverage: terminal.coverage,
        hits: Count(u32::try_from(terminal.edges.len()).unwrap_or(u32::MAX)),
        elapsed: None,
    };
    line(&mut out, &palette.dim(&format!("~lanes {}", lane_signal(&report))));
    out
}

// ---------------------------------------------------------------- health

fn health_text(health: &Health, options: &TextOptions<'_>) -> String {
    let palette = options.palette;
    let column = Capability::ALL
        .into_iter()
        .map(|capability| capability.label().len())
        .max()
        .unwrap_or(0)
        .saturating_add(2);
    let mut out = String::new();
    for (capability, state) in health.rows() {
        let label = capability.label();
        let pad = " ".repeat(column.saturating_sub(label.len()));
        let word = capability_slug(state);
        let rendered = match state {
            CapabilityState::Ready => palette.good(word),
            CapabilityState::Unreachable { detail } => {
                palette.fault(&format!("{word}: {detail}"))
            }
            CapabilityState::Unconfigured | CapabilityState::Detached => palette.dim(word),
        };
        let _ = writeln!(out, "{} {label}{pad}{rendered}", capability_glyph(state));
    }
    out
}

// ---------------------------------------------------------------- page failures

fn page_error_text(error: &PageError, options: &TextOptions<'_>) -> String {
    let value = page_fault(error, options);
    match error {
        PageError::Ambiguous(candidates) => {
            fault_with_evidence(&value, &candidate_lines(candidates), options)
        }
        _ => fault(&value, options),
    }
}

fn page_fault(error: &PageError, options: &TextOptions<'_>) -> Fault {
    match error {
        PageError::Resolve(resolve) => resolve_fault(resolve, options),
        PageError::Ambiguous(candidates) => ambiguity_fault(candidates.len(), options),
        PageError::Reopen(reopen) => Fault::new(
            "image-unreadable",
            reopen.package.to_string(),
            add_affordance(&reopen.package),
        )
        .detailed(reopen.detail.as_ref()),
        PageError::Projection(projection) => Fault::new(
            "projection-failed",
            options.subject.unwrap_or_default(),
            Affordance::None,
        )
        .detailed(projection_detail(projection)),
        PageError::KeyUnknown { key } => {
            Fault::new("key-unknown", key.to_string(), Affordance::Packages)
                .detailed("no loaded package holds a declaration with this key")
        }
    }
}

fn resolve_fault(error: &ResolveError, options: &TextOptions<'_>) -> Fault {
    match error {
        ResolveError::Parse { cause } => Fault::new(
            "bad-address",
            options.subject.unwrap_or_default(),
            Affordance::None,
        )
        .detailed(address_parse_detail(*cause)),
        ResolveError::PackageUnknown { package } => {
            let mut detail = String::from("the shelf has no entry for this package");
            let nearest = nearest_coordinates(package, options.neighbours);
            if !nearest.is_empty() {
                let _ = write!(detail, "; nearest on the shelf: {}", nearest.join(", "));
            }
            Fault::new(
                "package-unknown",
                package.to_string(),
                add_affordance(package),
            )
            .detailed(detail)
        }
        ResolveError::PackageNotReady { package } => Fault::new(
            "package-not-ready",
            package.to_string(),
            Affordance::Packages,
        )
        .detailed("the package is on the shelf but has no published image yet"),
    }
}

/// The shelf entries closest to a coordinate that is not on it, longest shared name first.
#[must_use]
pub fn nearest_coordinates(
    wanted: &PackageCoordinate,
    shelf: &[PackageCoordinate],
) -> Vec<String> {
    let mut scored: Vec<(usize, String)> = shelf
        .iter()
        .map(|candidate| {
            (
                shared_prefix(candidate.name.as_str(), wanted.name.as_str()),
                candidate.to_string(),
            )
        })
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    scored.into_iter().take(3).map(|(_, text)| text).collect()
}

/// Longest shared leading run of two spellings: the cheapest honest "nearest" ordering.
#[must_use]
pub fn shared_prefix(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

/// Reader-facing spelling of one address rejection.
#[must_use]
pub fn address_parse_detail(cause: AddressParseError) -> String {
    match cause {
        AddressParseError::Empty => "no address was supplied".to_owned(),
        AddressParseError::TooLong { observed, maximum } => {
            format!("the address is {observed} bytes; at most {maximum} are accepted")
        }
        AddressParseError::Coordinate { cause } => coordinate_parse_detail(cause).to_owned(),
        AddressParseError::Path { cause } => match cause {
            PathParseError::EmptySegment { index } => format!("path segment {index} is empty"),
            PathParseError::TooDeep { observed, maximum } => {
                format!("the path has {observed} segments; at most {maximum} are accepted")
            }
        },
        AddressParseError::Key { cause } => match cause {
            KeyParseError::Family { .. } => {
                "the key family is not thirty-two lower-hex characters".to_owned()
            }
            KeyParseError::Variant { .. } => {
                "the key variant is not thirty-two lower-hex characters".to_owned()
            }
        },
    }
}

/// Reader-facing spelling of one coordinate rejection.
#[must_use]
pub const fn coordinate_parse_detail(cause: CoordinateParseError) -> &'static str {
    match cause {
        CoordinateParseError::Empty => "no coordinate was supplied",
        CoordinateParseError::TooLong { .. } => "the coordinate exceeds the accepted length",
        CoordinateParseError::MissingEcosystem | CoordinateParseError::UnknownEcosystem { .. } => {
            "the ecosystem must be one of cargo npm pypi go maven nuget cpp"
        }
        CoordinateParseError::MissingVersion => "a coordinate is pinned: name@version",
        CoordinateParseError::EmptyName => "the package name is empty",
        CoordinateParseError::EmptyVersion => "the pinned version is empty",
        CoordinateParseError::Character { .. } => {
            "a byte cannot appear inside a coordinate name or version"
        }
    }
}

fn projection_detail(error: &ProjectionError) -> String {
    match error {
        ProjectionError::MissingEntity { .. } => {
            "the image holds no declaration at that coordinate".to_owned()
        }
        ProjectionError::MissingIdentity { .. } => {
            "the image holds no declaration identity for that coordinate".to_owned()
        }
        ProjectionError::MissingPool { pool, .. } => format!(
            "the image is missing its {} pool for that declaration",
            pool_label(*pool)
        ),
        ProjectionError::ParentageMismatch { .. } => {
            "a member disagreed with its owner about parentage".to_owned()
        }
        ProjectionError::ProseBudget {
            required, maximum, ..
        } => format!(
            "documentation needs {} bytes; {} were admitted",
            required.get(),
            maximum.get()
        ),
        ProjectionError::AddressDepth { .. } => {
            "the declaration nests deeper than an address can spell".to_owned()
        }
    }
}

const fn pool_label(pool: MissingPool) -> &'static str {
    match pool {
        MissingPool::Name => "name",
        MissingPool::Members => "member",
        MissingPool::Documentation => "documentation",
        MissingPool::Type => "type",
        MissingPool::Text => "text",
    }
}

// ---------------------------------------------------------------- text shaping

fn paragraph_text(inlines: &[Inline]) -> String {
    let mut text = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(run) | Inline::Code(run) => text.push_str(run.as_str()),
            Inline::Link { label, .. } => text.push_str(label.as_str()),
            Inline::Break => text.push(' '),
        }
    }
    text
}

/// Collapses a multi-line signature onto one line, for previews inside a list row.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Greedy wrap at `width` character cells; a word wider than the budget stands on its own line
/// rather than being cut, because a cut identifier is not an identifier.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && width_of(&current) + 1 + width_of(word) > width {
            rows.push(core::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows
}

/// Character cells a string occupies, counting one cell per scalar value.
///
/// No dependency may be added for a width table, and every glyph this renderer emits is one cell
/// wide, so counting scalars is exact for the text this module produces and close enough for
/// arbitrary documentation prose.
fn width_of(text: &str) -> usize {
    text.chars().count()
}

/// Truncates to `budget` cells with a trailing ellipsis. Addresses are never passed through this.
fn ellipsize(text: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    if width_of(text) <= budget {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(budget.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_palette_never_emits_an_escape_code() {
        let palette = Palette::plain();
        assert_eq!(palette.dim("x"), "x");
        assert_eq!(palette.fault("x"), "x");
        assert!(Palette::ansi().dim("x").contains('\u{1b}'));
    }

    #[test]
    fn prose_never_wraps_wider_than_the_prose_maximum() {
        assert_eq!(Width::new(200).prose(), 88);
        assert_eq!(Width::new(60).prose(), 60);
        assert_eq!(Width::new(10).columns(), 40);
    }

    #[test]
    fn ellipsis_only_appears_when_text_is_cut() {
        assert_eq!(ellipsize("abcdef", 6), "abcdef");
        assert_eq!(ellipsize("abcdef", 4), "abc\u{2026}");
    }

    #[test]
    fn a_detached_process_offers_the_one_move_it_has() {
        assert_eq!(
            TerminalAffordances::new(Attachment::Detached).spell(&Affordance::None),
            Some("run without --detached".to_owned())
        );
        assert_eq!(
            TerminalAffordances::new(Attachment::Attached).spell(&Affordance::None),
            None
        );
    }
}
