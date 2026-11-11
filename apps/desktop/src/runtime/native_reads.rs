//! Reads of the SQL, CSS and Impact pages.
//!
//! Each page is composed from the owner's public answers (`impact`,
//! `sql-query`, `sql-read-source`, `stylo-query`, `stylo-read-site`) and lowered,
//! on the read worker, into the sentences the page says. A reply that could not
//! look, looked in part, or looked at files which have changed is said to be so
//! in the model: no empty list is allowed to read as "nothing".

use super::actor::CancellationToken;
use super::reads::failure;
use crate::core::{ErrorValue, FaultCode};
use crate::model::browse::BrowseValue;
use crate::model::native::{
    Coverage, CssDeclaration, CssModel, CssRead, CssRule, CssState, DomainStateView, DomainView, FreshnessView,
    GapView, ImpactModel, ImpactRowView, ListEnd, NativeFind, NativeHit, NodeView, SeedView, Site, SqlColumn,
    SqlGapSite, SqlModel, SqlObject, SqlSite, SqlUse, Strength, UsersState, UsersView, Verified,
};
use crate::model::pages::{PackageRef, PageValue, ReadFailure, SearchQuery};
use crate::navigation::{ImpactRoute, NativeDomain, NativeKind, NativeSubject};
use backend_client::ClientError;
use backend_library::impact::{
    ImpactDirection, ImpactDomain, ImpactDomainState, ImpactFreshness, ImpactNodeView, ImpactReport, ImpactRequest,
    ImpactSeed, ImpactSeedSpec, ImpactSeedState, ImpactStrength, ImpactTerminal,
};
use backend_library::sql_query::{
    SqlAttemptState, SqlDefinitionAction, SqlFact, SqlFailure, SqlObjectKind, SqlQueryPage, SqlQueryRequest,
    SqlQueryRole, SqlSourceRead, SqlStatementSupport,
};
use backend_library::stylo_query::{
    StyloAttemptState, StyloFailure, StyloNameRole, StyloQueryHit, StyloQueryPage, StyloQueryRequest, StyloScopeGap,
    StyloSourceRead,
};
use backend_library::{PackageReference, SurfaceCommand, SurfaceReply};
use backend_present::{Engine, Probe};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Rows an Impact page asks for each time. The owner's own default.
const IMPACT_PAGE_ROWS: u16 = backend_library::impact::IMPACT_DEFAULT_PAGE_ROWS;
/// Rows of the other pages' lists.
const LIST_ROWS: u16 = 64;
/// Source reads one page may spend verifying its rows (each is one owner round trip).
const SOURCE_READS: usize = 48;
/// Projects one Find asks about SQL and CSS.
const FIND_PROJECTS: usize = 8;
/// Hits one Find shows for SQL and CSS.
const FIND_HITS: usize = 12;
/// The shortest query Find looks for in SQL and CSS.
const FIND_MIN_CHARS: usize = 2;

fn protocol(message: impl Into<String>) -> ReadFailure {
    ReadFailure::Fault(ErrorValue::new(FaultCode::Protocol, message.into()))
}

fn check(cancel: &CancellationToken) -> Result<(), ReadFailure> {
    if cancel.is_cancelled() {
        Err(ReadFailure::Cancelled)
    } else {
        Ok(())
    }
}

fn package_of(subject: &NativeSubject) -> Result<PackageReference, ReadFailure> {
    let coordinate = subject
        .project
        .service_coordinate()
        .map_err(|error| protocol(format!("the project path cannot be sent to the owner: {error:?}")))?;
    PackageRef::parse(coordinate)
        .map(|package| package.reference().clone())
        .map_err(|error| protocol(format!("the project is not a package the owner can name: {error}")))
}

/// Reads one SQL, CSS, Impact or Find-in-projects resource.
///
/// # Errors
/// The owner's failure to answer, as a typed fault.
pub fn compose(
    engine: &mut dyn Engine,
    key: &crate::model::browse::BrowseKey,
    cancel: &CancellationToken,
) -> Result<PageValue, ReadFailure> {
    use crate::model::browse::BrowseKey;
    let usage = QueryTimeLinks;
    match key {
        BrowseKey::Impact(route) => impact(engine, route, cancel).map(|model| PageValue::Browse(BrowseValue::Impact(Arc::new(model)))),
        BrowseKey::Sql(subject) => sql(engine, subject, &usage, cancel).map(|model| PageValue::Browse(BrowseValue::Sql(Arc::new(model)))),
        BrowseKey::Css(subject) => css(engine, subject, &usage, cancel).map(|model| PageValue::Browse(BrowseValue::Css(Arc::new(model)))),
        BrowseKey::FindNative(query) => find_native(engine, query, cancel).map(|model| PageValue::Browse(BrowseValue::FindNative(Arc::new(model)))),
        BrowseKey::Tree(_)
        | BrowseKey::CargoSourceInventory(_)
        | BrowseKey::CargoReadme(_)
        | BrowseKey::FindHome
        | BrowseKey::Find(_)
        | BrowseKey::Compare(_) => Err(protocol("this page is not read by the native reader")),
    }
}

// ---------------------------------------------------------------------------
// Impact
// ---------------------------------------------------------------------------

fn domain_of(domain: ImpactDomain) -> NativeDomain {
    match domain {
        ImpactDomain::Css => NativeDomain::Css,
        ImpactDomain::Code => NativeDomain::Code,
        ImpactDomain::Sql => NativeDomain::Sql,
    }
}

fn impact_domain(domain: NativeDomain) -> ImpactDomain {
    match domain {
        NativeDomain::Css => ImpactDomain::Css,
        NativeDomain::Code => ImpactDomain::Code,
        NativeDomain::Sql => ImpactDomain::Sql,
    }
}

fn impact_request(subject: &NativeSubject, package: PackageReference, depth: Option<u8>, rows: u16) -> ImpactRequest {
    let mut request = ImpactRequest::new(
        package,
        ImpactSeed::Named(ImpactSeedSpec {
            domain: impact_domain(subject.kind.domain()),
            kind: subject.kind.seed_kind().to_owned(),
            name: subject.name.to_string(),
            path: subject.path.as_ref().map(ToString::to_string),
        }),
    );
    request.direction = ImpactDirection::Dependents;
    if let Some(depth) = depth {
        request.depth = depth;
    }
    request.page_rows = rows;
    request
}

fn impact_page(engine: &mut dyn Engine, request: ImpactRequest) -> Result<ImpactReport, ReadFailure> {
    match engine.surface(SurfaceCommand::Impact { request }).map_err(|error| failure(&error))? {
        SurfaceReply::Impact(report) => Ok(report),
        _ => Err(protocol("the impact reply changed shape")),
    }
}

fn site_of(path: Option<&String>, line: Option<u32>) -> Option<Site> {
    path.map(|path| Site { path: Arc::from(path.as_str()), line })
}

fn node_view(subject: &NativeSubject, node: &ImpactNodeView, narrow: bool) -> NodeView {
    let domain = domain_of(node.domain);
    NodeView {
        domain,
        kind: Arc::from(node.kind.as_str()),
        name: Arc::from(node.name.as_str()),
        site: site_of(node.path.as_ref(), node.line),
        follow: follow_of(subject, node, narrow),
    }
}

/// A question can start from this node when it is a kind the owner can follow.
fn follow_of(subject: &NativeSubject, node: &ImpactNodeView, narrow: bool) -> Option<NativeSubject> {
    let domain = domain_of(node.domain);
    let kind = NativeKind::from_node(domain, &node.kind)?;
    // A file is named by its path; everything else by its name, narrowed to
    // its file only when the name alone was ambiguous.
    match kind {
        NativeKind::CssStylesheet | NativeKind::CodeFile => {
            NativeSubject::new(subject.project.clone(), kind, node.path.as_deref().unwrap_or(node.name.as_str()), None).ok()
        }
        _ => NativeSubject::new(subject.project.clone(), kind, node.name.as_str(), if narrow { node.path.as_deref() } else { None }).ok(),
    }
}

fn row_view(subject: &NativeSubject, row: &backend_library::impact::ImpactRow) -> ImpactRowView {
    ImpactRowView {
        n: row.n,
        depth: row.depth,
        strength: match row.strength {
            ImpactStrength::Exact => Strength::Exact,
            ImpactStrength::Candidate => Strength::Possible,
        },
        domain: domain_of(row.node.domain),
        kind: Arc::from(row.node.kind.as_str()),
        name: Arc::from(row.node.name.as_str()),
        site: site_of(row.node.path.as_ref(), row.node.line),
        within: row.node.within.as_deref().map(Arc::from),
        relation: Arc::from(row.relation.as_str()),
        why: Arc::from(row.why.as_str()),
        via: (row.via != 0).then_some(row.via),
        follow: follow_of(subject, &row.node, false),
    }
}

fn freshness_view(freshness: &ImpactFreshness) -> FreshnessView {
    match freshness {
        ImpactFreshness::Current { checked } => FreshnessView::Current { checked: *checked },
        ImpactFreshness::Stale { checked, changed, action } => FreshnessView::Stale {
            checked: *checked,
            changed: changed.iter().map(|path| Arc::from(path.as_str())).collect(),
            action: Arc::from(action.as_str()),
        },
        ImpactFreshness::Unknown { why } => FreshnessView::Unknown(Arc::from(why.as_str())),
    }
}

fn domain_views(report: &ImpactReport) -> Vec<DomainView> {
    report
        .domains
        .iter()
        .map(|basis| DomainView {
            domain: domain_of(basis.domain),
            files: basis.files,
            state: match &basis.state {
                ImpactDomainState::Analyzed => DomainStateView::Analyzed,
                ImpactDomainState::Partial { reasons } => {
                    DomainStateView::Partial(reasons.iter().map(|reason| Arc::from(reason.as_str())).collect())
                }
                ImpactDomainState::Unavailable { reason } => DomainStateView::Unavailable(Arc::from(reason.as_str())),
                ImpactDomainState::NotInstalled => DomainStateView::NotInstalled,
            },
        })
        .collect()
}

fn gap_views(report: &ImpactReport) -> Vec<GapView> {
    report
        .gaps
        .iter()
        .map(|gap| GapView { facet: Arc::from(gap.facet.as_str()), note: Arc::from(gap.note.as_str()), count: gap.count })
        .collect()
}

/// Lowers an impact answer. `rows` are every row of every page the view holds;
/// the last page's report supplies everything else.
pub(crate) fn impact_model(route: &ImpactRoute, last: &ImpactReport, rows: &[backend_library::impact::ImpactRow]) -> ImpactModel {
    let subject = &route.subject;
    let more = matches!(last.terminal, ImpactTerminal::More { .. });
    ImpactModel {
        subject: subject.clone(),
        seed: match &last.seed_state {
            ImpactSeedState::Resolved => SeedView::Resolved,
            ImpactSeedState::Ambiguous => SeedView::Ambiguous,
            ImpactSeedState::NotFound { why } => SeedView::NotFound(Arc::from(why.as_str())),
            ImpactSeedState::Unavailable { why } => SeedView::Unavailable(Arc::from(why.as_str())),
        },
        seeds: last.seeds.iter().map(|seed| node_view(subject, seed, matches!(last.seed_state, ImpactSeedState::Ambiguous))).collect(),
        exact: last.totals.exact,
        possible: last.totals.candidate,
        files: last.totals.files,
        by_kind: last.totals.by_kind.iter().map(|(kind, count)| (Arc::from(kind.as_str()), *count)).collect(),
        rows: rows.iter().map(|row| row_view(subject, row)).collect(),
        domains: domain_views(last),
        gaps: gap_views(last),
        freshness: freshness_view(&last.freshness),
        end: match &last.terminal {
            ImpactTerminal::Complete => ListEnd::Complete,
            ImpactTerminal::More { .. } if route.more().is_some() => ListEnd::More,
            ImpactTerminal::More { .. } => ListEnd::ViewFull,
            ImpactTerminal::BudgetLimited { budget } => ListEnd::BudgetLimited(Arc::from(budget.as_str())),
        },
        pages: route.pages,
        depth: last.depth,
        proves_no_impact: last.proves_no_impact() && !more,
        next: last.next.iter().map(|next| Arc::from(next.as_str())).collect(),
    }
}

fn impact(engine: &mut dyn Engine, route: &ImpactRoute, cancel: &CancellationToken) -> Result<ImpactModel, ReadFailure> {
    let package = package_of(&route.subject)?;
    let mut request = impact_request(&route.subject, package, None, IMPACT_PAGE_ROWS);
    let mut rows = Vec::new();
    let mut report;
    let mut page = 0u8;
    loop {
        check(cancel)?;
        report = impact_page(engine, request.clone())?;
        report.admit().map_err(|_| protocol("the impact reply was not self-consistent"))?;
        rows.extend(report.rows.iter().cloned());
        page += 1;
        match &report.terminal {
            ImpactTerminal::More { cursor } if page < route.pages => request.continuation = Some(cursor.clone()),
            _ => break,
        }
    }
    Ok(impact_model(route, &report, &rows))
}

// ---------------------------------------------------------------------------
// The interface a page asks "who uses this?" through
// ---------------------------------------------------------------------------

/// Components, queries and code that use something. Today the owner answers
/// from query-time links (its impact traversal over the analyses it holds);
/// when TypeScript-to-CSS links become graph edges, another implementation
/// answers the same question and the pages do not change.
pub(crate) trait UsageLinks {
    /// Who uses `subject`, as far as the owner can say.
    fn users(&self, engine: &mut dyn Engine, subject: &NativeSubject, cancel: &CancellationToken) -> Result<UsersView, ReadFailure>;
}

/// The owner's query-time links: its impact traversal, kept to the code that uses the thing.
pub(crate) struct QueryTimeLinks;

impl UsageLinks for QueryTimeLinks {
    fn users(&self, engine: &mut dyn Engine, subject: &NativeSubject, cancel: &CancellationToken) -> Result<UsersView, ReadFailure> {
        check(cancel)?;
        let package = package_of(subject)?;
        let request = impact_request(subject, package, None, 100);
        let report = match engine.surface(SurfaceCommand::Impact { request }) {
            Ok(SurfaceReply::Impact(report)) => report,
            Ok(_) => return Ok(UsersView::unavailable("the impact reply changed shape")),
            Err(ClientError::RequestCancelled(_)) => return Err(ReadFailure::Cancelled),
            Err(error) => return Ok(UsersView::unavailable(error.to_string())),
        };
        Ok(users_view(subject, &report))
    }
}

pub(crate) fn users_view(subject: &NativeSubject, report: &ImpactReport) -> UsersView {
    match &report.seed_state {
        ImpactSeedState::Unavailable { why } => return UsersView::unavailable(why.as_str()),
        ImpactSeedState::NotFound { .. } | ImpactSeedState::Ambiguous | ImpactSeedState::Resolved => {}
    }
    let rows: Vec<ImpactRowView> = report
        .rows
        .iter()
        .filter(|row| row.node.domain == ImpactDomain::Code || row.node.domain == ImpactDomain::Sql && row.node.kind == "query")
        .map(|row| row_view(subject, row))
        .collect();
    let exact = u32::try_from(rows.iter().filter(|row| row.strength == Strength::Exact).count()).unwrap_or(u32::MAX);
    let possible = u32::try_from(rows.len()).unwrap_or(u32::MAX).saturating_sub(exact);
    UsersView {
        state: UsersState::Read,
        rows,
        exact,
        possible,
        incomplete: !matches!(report.terminal, ImpactTerminal::Complete),
        gaps: gap_views(report),
        freshness: Some(freshness_view(&report.freshness)),
    }
}

// ---------------------------------------------------------------------------
// SQL
// ---------------------------------------------------------------------------

fn sql_request(package: &PackageReference, role: SqlQueryRole) -> SqlQueryRequest {
    SqlQueryRequest {
        package: package.clone(),
        role,
        name: None,
        kind: None,
        parent: None,
        uses: None,
        page_rows: LIST_ROWS,
        output_bytes: 1_048_576,
        scan_rows: 65_536,
        scan_bytes: 16 * 1024 * 1024,
        continuation: None,
    }
}

fn sql_page(engine: &mut dyn Engine, request: SqlQueryRequest) -> Result<SqlQueryPage, ReadFailure> {
    match engine.surface(SurfaceCommand::SqlQuery { request }).map_err(|error| failure(&error))? {
        SurfaceReply::SqlQuery(page) => Ok(page),
        _ => Err(protocol("the sql-query reply changed shape")),
    }
}

fn sql_failure_words(reason: SqlFailure) -> &'static str {
    match reason {
        SqlFailure::Read => "a file could not be read",
        SqlFailure::SelectionUnavailable => "the files to analyse could not be selected",
        SqlFailure::InvalidUtf8 => "a file is not valid UTF-8",
        SqlFailure::Limit => "a size limit was reached",
        SqlFailure::EngineNotAdmitted => "no SQL parser is installed for this build",
        SqlFailure::Deadline => "the analysis ran out of time",
        SqlFailure::Cancelled => "the analysis was cancelled",
        SqlFailure::WorkerFailure => "the SQL parser stopped",
        SqlFailure::UnsupportedDialect => "the dialect is not supported",
        SqlFailure::Context => "the project context could not be read",
    }
}

/// Whether the analysis stood behind a SQL page, and what it said about itself.
pub(crate) fn sql_coverage(page: &SqlQueryPage) -> Coverage {
    let basis = &page.basis;
    let mut notes: Vec<Arc<str>> = Vec::new();
    let mut analysed = true;
    let mut complete = true;
    match &basis.attempt {
        SqlAttemptState::LatestObserved => {}
        SqlAttemptState::NoSelectedAnalysis => {
            analysed = false;
            notes.push(Arc::from("SQL has not been analysed in this project. Index it to read its SQL."));
        }
        SqlAttemptState::ProjectNotSelected => {
            analysed = false;
            notes.push(Arc::from("This project is not the one the index has selected, so its SQL was not read."));
        }
        SqlAttemptState::RetainedAfterUnavailable { reason } => {
            complete = false;
            notes.push(Arc::from(format!(
                "The latest SQL analysis failed ({}); this is the last one that worked.",
                sql_failure_words(*reason)
            )));
        }
        SqlAttemptState::Unavailable { reason } => {
            analysed = false;
            notes.push(Arc::from(format!("SQL could not be analysed: {}.", sql_failure_words(*reason))));
        }
    }
    if analysed {
        if !basis.inventory_complete {
            complete = false;
            notes.push(Arc::from("Not every SQL file in the project could be listed, so some may be missing here."));
        }
        let gaps = &basis.gaps;
        for (count, one, many) in [
            (gaps.unavailable_hosts, "SQL file could not be read by the analysis, so what is in it is not shown.", "SQL files could not be read by the analysis, so what is in them is not shown."),
            (gaps.native_error_hosts, "SQL file was rejected by the parser, so what is in it is not shown.", "SQL files were rejected by the parser, so what is in them is not shown."),
            (gaps.partial_statements, "statement was only partly understood: a mention in it may be missing.", "statements were only partly understood: a mention in them may be missing."),
            (gaps.unsupported_statements, "statement was not understood at all: a mention in it may be missing.", "statements were not understood at all: a mention in them may be missing."),
        ] {
            if count > 0 {
                complete = false;
                notes.push(Arc::from(format!("{count} {}", if count == 1 { one } else { many })));
            }
        }
        notes.push(Arc::from(
            "This is what the project's SQL files say. Nothing was read from a running database.",
        ));
    } else {
        complete = false;
    }
    Coverage { analysed, complete, notes }
}

fn action_words(action: &SqlDefinitionAction) -> String {
    match action {
        SqlDefinitionAction::Create => "created".to_owned(),
        SqlDefinitionAction::Drop => "dropped".to_owned(),
        SqlDefinitionAction::AddColumn => "added as a column".to_owned(),
        SqlDefinitionAction::DropColumn => "dropped as a column".to_owned(),
        SqlDefinitionAction::Rename { to, .. } => format!("renamed to {to}"),
        SqlDefinitionAction::Alter => "altered".to_owned(),
    }
}

fn statement_words(kind: backend_semantic::sql_analysis::SqlStatementKind) -> &'static str {
    use backend_semantic::sql_analysis::SqlStatementKind as K;
    match kind {
        K::CreateTable => "CREATE TABLE",
        K::CreateView => "CREATE VIEW",
        K::CreateIndex => "CREATE INDEX",
        K::CreateTrigger => "CREATE TRIGGER",
        K::CreateFunction => "CREATE FUNCTION",
        K::CreateType => "CREATE TYPE",
        K::CreateSchema => "CREATE SCHEMA",
        K::CreateSequence => "CREATE SEQUENCE",
        K::AlterTable => "ALTER TABLE",
        K::Drop => "DROP",
        K::Select => "SELECT",
        K::Insert => "INSERT",
        K::Update => "UPDATE",
        K::Delete => "DELETE",
        K::Merge => "MERGE",
        K::Other => "a statement",
    }
}

fn gap_words(gap: backend_semantic::sql_analysis::SqlGap) -> &'static str {
    use backend_semantic::sql_analysis::SqlGap as G;
    match gap {
        G::OpaqueBody => "its body is opaque to the analysis",
        G::DynamicSql => "it builds SQL at run time",
        G::UnmodeledConstruct => "it uses a construct the analysis does not model",
        G::UnmodeledStatement => "it is a kind of statement the analysis does not model",
        G::Limit => "it hit an analysis limit",
    }
}

/// A source read's verdict on one hit, with its line and the exact text.
struct SourceRead {
    line: Option<u32>,
    text: Option<Arc<str>>,
    verified: Verified,
}

impl SourceRead {
    const UNREAD: Self = Self { line: None, text: None, verified: Verified::Unread };
}

/// Spends at most [`SOURCE_READS`] round trips on re-reading a page's rows.
struct Reads {
    left: usize,
}

impl Reads {
    fn new() -> Self {
        Self { left: SOURCE_READS }
    }

    fn sql(&mut self, engine: &mut dyn Engine, hit: &backend_library::sql_query::SqlQueryHit, cancel: &CancellationToken) -> Result<SourceRead, ReadFailure> {
        let Some(reference) = hit.source.clone() else { return Ok(SourceRead::UNREAD) };
        if self.left == 0 {
            return Ok(SourceRead::UNREAD);
        }
        self.left -= 1;
        check(cancel)?;
        match engine.surface(SurfaceCommand::SqlReadSource { reference }) {
            Ok(SurfaceReply::SqlReadSource(SqlSourceRead::Available { line_1, text, .. })) => {
                Ok(SourceRead { line: Some(line_1), text: Some(Arc::from(text.as_str())), verified: Verified::Matches })
            }
            Ok(SurfaceReply::SqlReadSource(SqlSourceRead::SourceChanged)) => {
                Ok(SourceRead { line: None, text: None, verified: Verified::Changed })
            }
            Ok(_) => Ok(SourceRead::UNREAD),
            Err(ClientError::RequestCancelled(_)) => Err(ReadFailure::Cancelled),
            Err(_) => Ok(SourceRead::UNREAD),
        }
    }

    fn stylo(&mut self, engine: &mut dyn Engine, reference: &backend_library::stylo_query::StyloSourceReference, cancel: &CancellationToken) -> Result<SourceRead, ReadFailure> {
        if self.left == 0 {
            return Ok(SourceRead::UNREAD);
        }
        self.left -= 1;
        check(cancel)?;
        match engine.surface(SurfaceCommand::StyloReadSite { reference: reference.clone() }) {
            Ok(SurfaceReply::StyloReadSite(StyloSourceRead::Available { line_1, text, .. })) => {
                Ok(SourceRead { line: Some(line_1), text: Some(Arc::from(text.as_str())), verified: Verified::Matches })
            }
            Ok(SurfaceReply::StyloReadSite(StyloSourceRead::SourceChanged)) => {
                Ok(SourceRead { line: None, text: None, verified: Verified::Changed })
            }
            Ok(_) => Ok(SourceRead::UNREAD),
            Err(ClientError::RequestCancelled(_)) => Err(ReadFailure::Cancelled),
            Err(_) => Ok(SourceRead::UNREAD),
        }
    }
}

fn is_sql_path(path: &str) -> bool {
    std::path::Path::new(path).extension().is_some_and(|extension| extension.eq_ignore_ascii_case("sql"))
}

fn object_kind(kind: SqlObjectKind) -> Option<NativeKind> {
    match kind {
        SqlObjectKind::Table => Some(NativeKind::SqlTable),
        SqlObjectKind::View => Some(NativeKind::SqlView),
        _ => None,
    }
}

/// A definition query by a name, retried in lower case when the owner's
/// dialect folding would have met it there.
fn sql_by_name(
    engine: &mut dyn Engine,
    mut request: SqlQueryRequest,
    name: &str,
    cancel: &CancellationToken,
) -> Result<SqlQueryPage, ReadFailure> {
    check(cancel)?;
    request.name = Some(name.to_owned());
    let page = sql_page(engine, request.clone())?;
    if page.hits.is_empty() && name.chars().any(|c| c.is_ascii_uppercase()) {
        check(cancel)?;
        request.name = Some(name.to_ascii_lowercase());
        return sql_page(engine, request);
    }
    Ok(page)
}

fn definition_of(fact: &SqlFact) -> Option<&backend_semantic::sql_analysis::SqlDefinition> {
    match fact {
        SqlFact::Definition(definition) => Some(definition),
        _ => None,
    }
}

fn sql(engine: &mut dyn Engine, subject: &NativeSubject, usage: &dyn UsageLinks, cancel: &CancellationToken) -> Result<SqlModel, ReadFailure> {
    let package = package_of(subject)?;
    let (object_name, column) = match subject.kind {
        NativeKind::SqlColumn => match subject.name.rsplit_once('.') {
            Some((table, column)) => (table.to_owned(), Some(column.to_owned())),
            None => (subject.name.to_string(), None),
        },
        _ => (subject.name.to_string(), None),
    };
    let table_subject = subject.table_of_column().unwrap_or_else(|| subject.clone());
    // The object itself, then its columns, then every written mention.
    let mut by_name = sql_request(&package, SqlQueryRole::Definition);
    by_name.kind = None;
    let object_page = sql_by_name(engine, by_name, &object_name, cancel)?;
    let coverage = sql_coverage(&object_page);
    let mut reads = Reads::new();
    let mut definitions = Vec::new();
    let mut kind = None;
    let mut live = false;
    let mut dialect = Arc::<str>::from("SQL");
    let mut changed: BTreeSet<Arc<str>> = BTreeSet::new();
    // Nothing is read from a project nobody analysed, whatever the page holds.
    let hits = if coverage.analysed { object_page.hits.as_slice() } else { &[] };
    for hit in hits {
        let Some(definition) = definition_of(&hit.fact) else { continue };
        let Some(native) = object_kind(definition.kind) else { continue };
        kind = kind.or(Some(native));
        live = definition.live_at_head;
        dialect = Arc::from(match hit.dialect {
            backend_semantic::sql_analysis::SqlDialect::Sqlite => "SQLite",
            backend_semantic::sql_analysis::SqlDialect::Postgres => "PostgreSQL",
        });
        let read = reads.sql(engine, hit, cancel)?;
        if read.verified == Verified::Changed {
            changed.insert(Arc::from(hit.path.as_str()));
        }
        definitions.push(SqlSite {
            site: Site { path: Arc::from(hit.path.as_str()), line: read.line },
            action: Arc::from(action_words(&definition.action)),
            written: read.text,
            live: definition.live_at_head,
            verified: read.verified,
        });
    }
    let object = match (&coverage, kind) {
        (Coverage { analysed: false, notes, .. }, _) => SqlObject::Unavailable(notes.first().cloned().unwrap_or_else(|| Arc::from("SQL was not analysed."))),
        (_, Some(kind)) => SqlObject::Defined { kind, live, dialect },
        (_, None) => SqlObject::NotDefined,
    };
    let analysed = coverage.analysed;

    let mut columns: Vec<SqlColumn> = Vec::new();
    let mut uses = Vec::new();
    let mut uses_incomplete = false;
    let mut gap_sites = Vec::new();
    if analysed && matches!(object, SqlObject::Defined { .. }) {
        check(cancel)?;
        let mut request = sql_request(&package, SqlQueryRole::Definition);
        request.kind = Some(SqlObjectKind::Column);
        request.parent = Some(object_name.clone());
        let mut page = sql_page(engine, request.clone())?;
        if page.hits.is_empty() && object_name.chars().any(|c| c.is_ascii_uppercase()) {
            request.parent = Some(object_name.to_ascii_lowercase());
            page = sql_page(engine, request)?;
        }
        for hit in &page.hits {
            let Some(definition) = definition_of(&hit.fact) else { continue };
            let read = reads.sql(engine, hit, cancel)?;
            if read.verified == Verified::Changed {
                changed.insert(Arc::from(hit.path.as_str()));
            }
            let name = definition.head_name.clone().unwrap_or_else(|| definition.name.clone());
            let site = SqlSite {
                site: Site { path: Arc::from(hit.path.as_str()), line: read.line },
                action: Arc::from(action_words(&definition.action)),
                written: read.text,
                live: definition.live_at_head,
                verified: read.verified,
            };
            match columns.iter_mut().find(|column| *column.name == *name) {
                Some(existing) => {
                    existing.live = definition.live_at_head;
                    existing.sites.push(site);
                }
                None => columns.push(SqlColumn { name: Arc::from(name), live: definition.live_at_head, sites: vec![site] }),
            }
        }

        check(cancel)?;
        let mut request = sql_request(&package, SqlQueryRole::Reference);
        request.uses = Some(table_name_for_uses(&object_name, column.as_deref()));
        let mut page = sql_page(engine, request.clone())?;
        if page.hits.is_empty() && object_name.chars().any(|c| c.is_ascii_uppercase()) {
            request.uses = Some(table_name_for_uses(&object_name.to_ascii_lowercase(), column.as_deref().map(str::to_ascii_lowercase).as_deref()));
            page = sql_page(engine, request)?;
        }
        uses_incomplete = !matches!(page.terminal, backend_library::sql_query::SqlQueryTerminal::Complete);
        for hit in &page.hits {
            let SqlFact::Reference(reference) = &hit.fact else { continue };
            let read = reads.sql(engine, hit, cancel)?;
            if read.verified == Verified::Changed {
                changed.insert(Arc::from(hit.path.as_str()));
            }
            use backend_semantic::sql_analysis::{SqlRefKind, SqlRefRole, SqlResolution};
            let live = match &reference.resolution {
                SqlResolution::Resolved { live, .. } | SqlResolution::Relation { live, .. } => *live,
                _ => true,
            };
            uses.push(SqlUse {
                site: Site { path: Arc::from(hit.path.as_str()), line: read.line },
                statement: Arc::from(statement_words(reference.statement_kind)),
                role: Arc::from(match reference.role {
                    SqlRefRole::Read => "reads",
                    SqlRefRole::Write => "writes",
                    SqlRefRole::Schema => "defines or alters",
                }),
                column: (reference.kind == SqlRefKind::Column).then(|| Arc::from(reference.name.as_str())),
                within: reference.in_object.as_ref().map(|object| Arc::from(format!("{} {}", sql_object_word(object.kind), object.name))),
                live,
                in_code: !is_sql_path(&hit.path),
                verified: read.verified,
            });
        }

        check(cancel)?;
        let mut request = sql_request(&package, SqlQueryRole::Gap);
        request.page_rows = 24;
        let gaps = sql_page(engine, request)?;
        for hit in &gaps.hits {
            if let SqlFact::Statement { class, support, .. } = &hit.fact {
                let why = match support {
                    SqlStatementSupport::Full => continue,
                    SqlStatementSupport::Partial(gap) => format!("a {} statement was only partly read: {}", statement_words(*class), gap_words(*gap)),
                    SqlStatementSupport::Unsupported(gap) => format!("a {} statement was not read: {}", statement_words(*class), gap_words(*gap)),
                };
                gap_sites.push(SqlGapSite { path: Arc::from(hit.path.as_str()), why: Arc::from(why) });
            }
        }
    }
    check(cancel)?;
    let users = if analysed && matches!(object, SqlObject::Defined { .. }) {
        usage.users(engine, &table_subject, cancel)?
    } else {
        UsersView::unavailable("the SQL analysis has nothing to follow")
    };
    Ok(SqlModel {
        subject: subject.clone(),
        object_name: Arc::from(object_name),
        column: column.map(Arc::from),
        object,
        definitions,
        columns,
        uses,
        uses_incomplete,
        users,
        coverage,
        gap_sites,
        changed: changed.into_iter().collect(),
    })
}

fn table_name_for_uses(table: &str, column: Option<&str>) -> String {
    match column {
        Some(column) => format!("{table}.{column}"),
        None => table.to_owned(),
    }
}

fn sql_object_word(kind: SqlObjectKind) -> &'static str {
    match kind {
        SqlObjectKind::Schema => "schema",
        SqlObjectKind::Table => "table",
        SqlObjectKind::View => "view",
        SqlObjectKind::Column => "column",
        SqlObjectKind::Index => "index",
        SqlObjectKind::Trigger => "trigger",
        SqlObjectKind::Function => "function",
        SqlObjectKind::Type => "type",
        SqlObjectKind::Sequence => "sequence",
    }
}

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

fn stylo_request(package: &PackageReference, role: StyloNameRole, name: &str) -> StyloQueryRequest {
    StyloQueryRequest {
        package: package.clone(),
        role,
        name: name.to_owned(),
        page_rows: LIST_ROWS,
        output_bytes: 1_048_576,
        scan_rows: 4096,
        scan_bytes: 4 * 1024 * 1024,
        continuation: None,
    }
}

fn stylo_page(engine: &mut dyn Engine, request: StyloQueryRequest) -> Result<StyloQueryPage, ReadFailure> {
    match engine.surface(SurfaceCommand::StyloQuery { request }).map_err(|error| failure(&error))? {
        SurfaceReply::StyloQuery(page) => Ok(page),
        _ => Err(protocol("the stylo-query reply changed shape")),
    }
}

fn stylo_failure_words(reason: StyloFailure) -> &'static str {
    match reason {
        StyloFailure::ReadUnavailable => "a file could not be read",
        StyloFailure::SelectionUnavailable => "the stylesheets to analyse could not be selected",
        StyloFailure::InvalidUtf8 => "a stylesheet is not valid UTF-8",
        StyloFailure::Limit => "a size limit was reached",
        StyloFailure::ParserUnavailable => "the CSS parser is not installed for this build",
        StyloFailure::UnsupportedHardLimit => "a stylesheet exceeds what the parser supports",
        StyloFailure::Deadline => "the analysis ran out of time",
        StyloFailure::WorkerFailure => "the CSS parser stopped",
        StyloFailure::PreferencesChanged => "the parser's settings changed during the analysis",
    }
}

fn scope_gap_words(gap: StyloScopeGap) -> &'static str {
    match gap {
        StyloScopeGap::AuthoredSourcePartial => "Some authored source is only partly observed.",
        StyloScopeGap::AuthoredReferenceSitesUnavailable => "Where a class is named in markup or scripts is not part of the stylesheet analysis.",
        StyloScopeGap::OrdinaryVariableReferencesUnavailable => "Not every var() reference to a custom property is located in the source.",
        StyloScopeGap::NativeDeclarationNameDecodeUnavailable => "Some declaration names could not be decoded.",
        StyloScopeGap::PreferenceStabilityUnknown => "The parser's settings could not be certified stable.",
        StyloScopeGap::ImportsNotResolved => "@import is not followed, so rules in imported files are not listed.",
        StyloScopeGap::AncestorRuleContextNotReturned => "A rule nested in another does not name the rule around it.",
        StyloScopeGap::FrameworkNotInterpreted => "Class names a framework builds or generates are not interpreted.",
        StyloScopeGap::DomAndCascadeNotAttempted => "Which elements a rule matches, and which rule wins the cascade, are not worked out.",
        StyloScopeGap::ProviderUnknown => "Which declaration supplies a custom property's value is not worked out.",
    }
}

pub(crate) fn css_coverage(page: &StyloQueryPage) -> Coverage {
    let basis = &page.basis;
    let mut notes: Vec<Arc<str>> = Vec::new();
    let mut analysed = true;
    let mut complete = true;
    match &basis.attempt {
        StyloAttemptState::LatestObserved => {}
        StyloAttemptState::NoSelectedAnalysis => {
            analysed = false;
            notes.push(Arc::from("CSS has not been analysed in this project. Index it to read its stylesheets."));
        }
        StyloAttemptState::ProjectNotSelected => {
            analysed = false;
            notes.push(Arc::from("This project is not the one the index has selected, so its stylesheets were not read."));
        }
        StyloAttemptState::UnknownLegacy => {
            analysed = false;
            notes.push(Arc::from("This project's CSS analysis comes from an older index and cannot be trusted. Index it again."));
        }
        StyloAttemptState::RetainedAfterUnavailable { reason } => {
            complete = false;
            notes.push(Arc::from(format!(
                "The latest CSS analysis failed ({}); this is the last one that worked.",
                stylo_failure_words(*reason)
            )));
        }
        StyloAttemptState::Unavailable { reason } => {
            analysed = false;
            notes.push(Arc::from(format!("CSS could not be analysed: {}.", stylo_failure_words(*reason))));
        }
    }
    if analysed {
        if !basis.selection_complete {
            complete = false;
            notes.push(Arc::from("Not every stylesheet in the project was selected for analysis, so some may be missing here."));
        }
        if basis.source_partial {
            complete = false;
            notes.push(Arc::from("Some stylesheet syntax was only partly read."));
        }
        // The owner always declares what stylesheet analysis cannot see.
        if !basis.gaps.is_empty() {
            complete = false;
        }
        for gap in &basis.gaps {
            notes.push(Arc::from(scope_gap_words(*gap)));
        }
    } else {
        complete = false;
    }
    Coverage { analysed, complete, notes }
}

fn rule_line(rule: &backend_semantic::stylo_analysis::StyloRule) -> (Option<u32>, bool) {
    rule.location.as_ref().map_or((None, false), |location| (Some(location.line.saturating_add(1)), location.approximate))
}

fn css(engine: &mut dyn Engine, subject: &NativeSubject, usage: &dyn UsageLinks, cancel: &CancellationToken) -> Result<CssModel, ReadFailure> {
    let package = package_of(subject)?;
    let role = match subject.kind {
        NativeKind::CssCustomProperty => StyloNameRole::CustomProperty,
        NativeKind::CssClass => StyloNameRole::Class,
        _ => return Err(protocol("the CSS page reads classes and custom properties only")),
    };
    check(cancel)?;
    let page = stylo_page(engine, stylo_request(&package, role, &subject.name))?;
    let coverage = css_coverage(&page);
    let mut reads = Reads::new();
    let mut rules: Vec<CssRule> = Vec::new();
    let mut declarations: Vec<CssDeclaration> = Vec::new();
    let mut property_reads: Vec<CssRead> = Vec::new();
    let mut changed: BTreeSet<Arc<str>> = BTreeSet::new();
    let mut covered: BTreeSet<(String, Vec<u32>, u32)> = BTreeSet::new();
    let mut declared: BTreeSet<(String, Vec<u32>)> = BTreeSet::new();
    // Authored sites first: they carry the exact source.
    for hit in &page.hits {
        let StyloQueryHit::SourceSite { path, site, rule, selector, source, .. } = hit else { continue };
        let read = reads.stylo(engine, source, cancel)?;
        if read.verified == Verified::Changed {
            changed.insert(Arc::from(path.as_str()));
        }
        let (approx_line, approximate) = rule_line(rule);
        let line = read.line.or(approx_line);
        let approximate = read.line.is_none() && approximate;
        match role {
            StyloNameRole::Class => {
                covered.insert((path.clone(), rule.path.clone(), site.selector_ordinal.unwrap_or(0)));
                rules.push(CssRule {
                    site: Site { path: Arc::from(path.as_str()), line },
                    approximate,
                    selector: Arc::from(selector.as_ref().map_or(format!(".{}", subject.name), |selector| selector.canonical.clone())),
                    condition: rule.condition.as_deref().map(Arc::from),
                    written: read.text,
                    verified: read.verified,
                });
            }
            StyloNameRole::CustomProperty => {
                declared.insert((path.clone(), rule.path.clone()));
                declarations.push(CssDeclaration {
                    site: Site { path: Arc::from(path.as_str()), line },
                    value: None,
                    important: false,
                    selector: None,
                    written: read.text,
                    verified: read.verified,
                });
            }
        }
    }
    for hit in &page.hits {
        match hit {
            StyloQueryHit::NativeSelector { path, rule, selector, .. } if role == StyloNameRole::Class => {
                if covered.contains(&(path.clone(), rule.path.clone(), selector.ordinal)) {
                    continue;
                }
                let (line, approximate) = rule_line(rule);
                rules.push(CssRule {
                    site: Site { path: Arc::from(path.as_str()), line },
                    approximate,
                    selector: Arc::from(selector.canonical.as_str()),
                    condition: rule.condition.as_deref().map(Arc::from),
                    written: None,
                    verified: Verified::Unread,
                });
            }
            StyloQueryHit::NativeDeclaration { path, rule, declaration, .. } if role == StyloNameRole::CustomProperty => {
                let value = declaration.value_available.then(|| Arc::from(declaration.value.as_str()));
                // Join the value to its authored site when one stands for the same rule.
                if let Some(existing) = declarations
                    .iter_mut()
                    .find(|existing| *existing.site.path == **path && existing.value.is_none() && declared.contains(&(path.clone(), rule.path.clone())))
                {
                    existing.value = value;
                    existing.important = declaration.important;
                    continue;
                }
                let (line, _) = rule_line(rule);
                declarations.push(CssDeclaration {
                    site: Site { path: Arc::from(path.as_str()), line },
                    value,
                    important: declaration.important,
                    selector: None,
                    written: None,
                    verified: Verified::Unread,
                });
            }
            StyloQueryHit::NativeDependency { path, rule, declaration, dependency, .. } if role == StyloNameRole::CustomProperty => {
                let (line, _) = rule_line(rule);
                property_reads.push(CssRead {
                    site: Site { path: Arc::from(path.as_str()), line },
                    property: Arc::from(declaration.property.as_str()),
                    how: Arc::from(dependency.kind.as_str()),
                });
            }
            _ => {}
        }
    }
    let found = !rules.is_empty() || !declarations.is_empty() || !property_reads.is_empty();
    let state = if !coverage.analysed {
        CssState::Unavailable(coverage.notes.first().cloned().unwrap_or_else(|| Arc::from("CSS was not analysed.")))
    } else if found {
        CssState::Found
    } else {
        CssState::NotFound
    };
    let incomplete = !matches!(page.terminal, backend_library::stylo_query::StyloQueryTerminal::Complete);
    check(cancel)?;
    let users = if coverage.analysed {
        usage.users(engine, subject, cancel)?
    } else {
        UsersView::unavailable("the CSS analysis has nothing to follow")
    };
    Ok(CssModel {
        subject: subject.clone(),
        state,
        rules,
        declarations,
        reads: property_reads,
        incomplete,
        users,
        coverage,
        changed: changed.into_iter().collect(),
    })
}

// ---------------------------------------------------------------------------
// Find: what the projects' SQL and CSS hold under a name
// ---------------------------------------------------------------------------

fn is_sql_name(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '$'))
        && !text.starts_with('.')
        && !text.ends_with('.')
        && text.matches('.').count() <= 1
}

fn is_css_name(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
}

/// The local projects the index holds (not the registry trees it unpacked).
fn indexed_projects(engine: &mut dyn Engine) -> Vec<crate::core::LocalProjectId> {
    let Ok(reply) = engine.probe(Probe::Packages) else { return Vec::new() };
    let backend_library::CommandReply::Packages(snapshot) = reply.reply else { return Vec::new() };
    let registry = crate::host::registry::composed();
    let mut projects = Vec::new();
    for row in snapshot.root.rows() {
        if !matches!(row.id, backend_library::RowId::Package(_)) {
            continue;
        }
        let Ok(package) = PackageRef::parse(&row.label) else { continue };
        if !package.is_local() || !std::path::Path::new(package.as_str()).is_absolute() {
            continue;
        }
        if registry.as_ref().is_some_and(|composed| composed.source.release_of(std::path::Path::new(package.as_str())).is_some()) {
            continue;
        }
        if let Ok(project) = crate::core::LocalProjectId::new(package.as_str()) {
            if !projects.contains(&project) {
                projects.push(project);
            }
        }
        if projects.len() == FIND_PROJECTS {
            break;
        }
    }
    projects
}

fn find_native(engine: &mut dyn Engine, query: &SearchQuery, cancel: &CancellationToken) -> Result<NativeFind, ReadFailure> {
    let text = query.text.trim();
    if text.chars().count() < FIND_MIN_CHARS {
        return Ok(NativeFind::default());
    }
    check(cancel)?;
    let projects = indexed_projects(engine);
    let mut find = NativeFind { asked: true, searched: projects.len(), ..NativeFind::default() };
    let mut unanalysed_sql = Vec::new();
    let mut unanalysed_css = Vec::new();
    let mut reads = Reads::new();
    for project in projects {
        check(cancel)?;
        let name = project
            .path()
            .file_name()
            .map_or_else(|| project.display_lossy(), |name| name.to_string_lossy().into_owned());
        let package = match package_of(&NativeSubject {
            project: project.clone(),
            kind: NativeKind::SqlTable,
            name: Arc::from("x"),
            path: None,
        }) {
            Ok(package) => package,
            Err(_) => continue,
        };
        if is_sql_name(text) {
            let (table, column) = match text.split_once('.') {
                Some((table, column)) => (table, Some(column)),
                None => (text, None),
            };
            let mut request = sql_request(&package, SqlQueryRole::Definition);
            request.page_rows = 16;
            match column {
                Some(column) => {
                    request.kind = Some(SqlObjectKind::Column);
                    request.parent = Some(table.to_owned());
                    request.name = Some(column.to_owned());
                }
                None => request.name = Some(text.to_owned()),
            }
            match sql_page(engine, request) {
                Ok(page) => {
                    let coverage = sql_coverage(&page);
                    if !coverage.analysed {
                        if matches!(page.basis.attempt, SqlAttemptState::Unavailable { .. }) {
                            find.notes.push(Arc::from(format!("SQL could not be searched in {name}: {}", coverage.notes.first().map_or("", |note| note))));
                        } else {
                            unanalysed_sql.push(name.clone());
                        }
                    }
                    let mut seen = BTreeSet::new();
                    for hit in &page.hits {
                        let Some(definition) = definition_of(&hit.fact) else { continue };
                        let kind = match definition.kind {
                            SqlObjectKind::Table => NativeKind::SqlTable,
                            SqlObjectKind::View => NativeKind::SqlView,
                            SqlObjectKind::Column => NativeKind::SqlColumn,
                            _ => continue,
                        };
                        let written = if kind == NativeKind::SqlColumn {
                            format!("{}.{}", definition.head_parent.clone().or_else(|| definition.parent.clone()).unwrap_or_default(), definition.head_name.clone().unwrap_or_else(|| definition.name.clone()))
                        } else {
                            definition.head_name.clone().unwrap_or_else(|| definition.name.clone())
                        };
                        if !seen.insert((kind, written.clone())) || find.hits.len() >= FIND_HITS {
                            continue;
                        }
                        let Ok(subject) = NativeSubject::new(project.clone(), kind, &written, None) else { continue };
                        let read = reads.sql(engine, hit, cancel)?;
                        find.hits.push(NativeHit {
                            subject,
                            site: Some(Site { path: Arc::from(hit.path.as_str()), line: read.line }),
                            detail: Arc::from(format!(
                                "{} in {name}",
                                match kind {
                                    NativeKind::SqlTable => "SQL table",
                                    NativeKind::SqlView => "SQL view",
                                    _ => "SQL column",
                                }
                            )),
                        });
                    }
                }
                Err(ReadFailure::Cancelled) => return Err(ReadFailure::Cancelled),
                Err(error) => find.notes.push(Arc::from(format!("SQL could not be searched in {name}: {}", failure_words(&error)))),
            }
        }
        let css_name = text.strip_prefix('.').unwrap_or(text);
        let role = if css_name.starts_with("--") { Some(NativeKind::CssCustomProperty) } else if is_css_name(css_name) { Some(NativeKind::CssClass) } else { None };
        if let Some(kind) = role {
            check(cancel)?;
            let Ok(subject) = NativeSubject::new(project.clone(), kind, css_name, None) else { continue };
            let role = if kind == NativeKind::CssCustomProperty { StyloNameRole::CustomProperty } else { StyloNameRole::Class };
            let mut request = stylo_request(&package, role, &subject.name);
            request.page_rows = 8;
            match stylo_page(engine, request) {
                Ok(page) => {
                    let coverage = css_coverage(&page);
                    if !coverage.analysed {
                        if matches!(page.basis.attempt, StyloAttemptState::Unavailable { .. } | StyloAttemptState::UnknownLegacy) {
                            find.notes.push(Arc::from(format!("CSS could not be searched in {name}: {}", coverage.notes.first().map_or("", |note| note))));
                        } else {
                            unanalysed_css.push(name.clone());
                        }
                    }
                    let first = page.hits.iter().find_map(|hit| match hit {
                        StyloQueryHit::SourceSite { path, rule, source, .. } => Some((path.clone(), rule_line(rule).0, Some(source.clone()))),
                        StyloQueryHit::NativeSelector { path, rule, .. }
                        | StyloQueryHit::NativeDeclaration { path, rule, .. }
                        | StyloQueryHit::NativeDependency { path, rule, .. } => Some((path.clone(), rule_line(rule).0, None)),
                    });
                    if let Some((path, approximate, source)) = first {
                        let line = match source {
                            Some(source) => reads.stylo(engine, &source, cancel)?.line.or(approximate),
                            None => approximate,
                        };
                        if find.hits.len() < FIND_HITS {
                            find.hits.push(NativeHit {
                                subject,
                                site: Some(Site { path: Arc::from(path.as_str()), line }),
                                detail: Arc::from(format!(
                                    "{} in {name}",
                                    if kind == NativeKind::CssCustomProperty { "CSS custom property" } else { "CSS class" }
                                )),
                            });
                        }
                    }
                }
                Err(ReadFailure::Cancelled) => return Err(ReadFailure::Cancelled),
                Err(error) => find.notes.push(Arc::from(format!("CSS could not be searched in {name}: {}", failure_words(&error)))),
            }
        }
    }
    if !unanalysed_sql.is_empty() {
        find.notes.push(Arc::from(format!("SQL has not been analysed in {}, so it was not searched there.", join_names(&unanalysed_sql))));
    }
    if !unanalysed_css.is_empty() {
        find.notes.push(Arc::from(format!("CSS has not been analysed in {}, so it was not searched there.", join_names(&unanalysed_css))));
    }
    find.notes.truncate(6);
    Ok(find)
}

fn failure_words(error: &ReadFailure) -> String {
    match error {
        ReadFailure::Fault(error) => error.message().to_owned(),
        ReadFailure::Cancelled => "cancelled".to_owned(),
        ReadFailure::Unavailable(_, why) => why.to_string(),
    }
}

fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests;
