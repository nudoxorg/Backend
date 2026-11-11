//! The SQL, CSS and Impact reads, lowered from the owner's replies. A scripted
//! engine answers each public command, and the tests read what the page would
//! say: the strength of a row, what the analysis could not see, a stale file,
//! a project nobody analysed. (Component tests of the read lane: the real
//! owner journey is in `shell::native_tests`.)

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::core::LocalProjectId;
use crate::model::native::Verdict;
use backend_library::impact::{
    ImpactCursor, ImpactCursorClaims, ImpactDomainBasis, ImpactGap, ImpactRow, ImpactTotals,
};
use backend_library::sql_query::{
    SqlHostBasis, SqlHostHealth, SqlNativeTerminal, SqlQueryBasis, SqlQueryHit, SqlQueryTerminal, SqlSourceClaims,
    SqlSourcePolicy, SqlSourceReference,
};
use backend_library::stylo_query::{
    StyloHostTerminalSummary, StyloQueryBasis, StyloQueryTerminal, StyloSourceClaims, StyloSourceReference,
    StyloWorkerCapability,
};
use backend_semantic::sql_analysis::{
    SqlByteRange, SqlCoverage, SqlDefinition, SqlDialect, SqlEntityRef, SqlGapCounts, SqlRefKind, SqlRefRole,
    SqlReference, SqlResolution, SqlStatementKind,
};
use backend_semantic::stylo_analysis::{
    StyloDeclaration, StyloDeclarationKind, StyloDependencyCoverage, StyloLocation, StyloRule, StyloRuleKind,
    StyloSelector, StyloSelectorComponent, StyloSelectorComponentKind, StyloSourceDisposition, StyloSourceOccurrence,
    StyloSourceRole, StyloVariableDependency,
};

type Script = Box<dyn FnMut(&SurfaceCommand) -> Result<SurfaceReply, ClientError>>;

/// An engine that answers each public command from a script and remembers what it was asked.
struct Scripted {
    script: Script,
    asked: Vec<String>,
}

impl Scripted {
    fn new(script: impl FnMut(&SurfaceCommand) -> Result<SurfaceReply, ClientError> + 'static) -> Self {
        Self { script: Box::new(script), asked: Vec::new() }
    }
}

impl Engine for Scripted {
    fn revision(&mut self) -> Result<backend_library::ViewStateRoot, ClientError> {
        Err(ClientError::Protocol("scripted".into()))
    }
    fn health(&mut self) -> Result<backend_library::HealthReport, ClientError> {
        Err(ClientError::Protocol("scripted".into()))
    }
    fn probe(&mut self, _: Probe<'_>) -> Result<backend_library::ReplyDto, ClientError> {
        Err(ClientError::Protocol("scripted: the packages were not asked".into()))
    }
    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        self.asked.push(match &command {
            SurfaceCommand::Impact { .. } => "impact".to_owned(),
            SurfaceCommand::SqlQuery { request } => format!("sql-query {:?}", request.role),
            SurfaceCommand::SqlReadSource { .. } => "sql-read-source".to_owned(),
            SurfaceCommand::StyloQuery { request } => format!("stylo-query {:?}", request.role),
            SurfaceCommand::StyloReadSite { .. } => "stylo-read-site".to_owned(),
            other => format!("{other:?}"),
        });
        (self.script)(&command)
    }
}

fn project() -> LocalProjectId {
    LocalProjectId::new("/workspace/shop").expect("project")
}

fn subject(kind: NativeKind, name: &str) -> NativeSubject {
    NativeSubject::new(project(), kind, name, None).expect("subject")
}

fn node(domain: ImpactDomain, kind: &str, name: &str, path: Option<&str>, line: Option<u32>) -> ImpactNodeView {
    ImpactNodeView {
        domain,
        kind: kind.to_owned(),
        name: name.to_owned(),
        path: path.map(str::to_owned),
        line,
        within: None,
        handle: None,
    }
}

fn row(n: u32, via: u32, strength: ImpactStrength, node: ImpactNodeView, relation: &str, why: &str) -> ImpactRow {
    ImpactRow { n, depth: 1, strength, node, relation: relation.to_owned(), why: why.to_owned(), via, read: None }
}

fn analysed(domain: ImpactDomain, files: u32) -> ImpactDomainBasis {
    ImpactDomainBasis { domain, state: ImpactDomainState::Analyzed, files }
}

fn report(rows: Vec<ImpactRow>) -> ImpactReport {
    let exact = rows.iter().filter(|row| row.strength == ImpactStrength::Exact).count() as u32;
    let candidate = rows.len() as u32 - exact;
    ImpactReport {
        basis: "abc123".into(),
        domains: vec![analysed(ImpactDomain::Css, 2), analysed(ImpactDomain::Code, 4), analysed(ImpactDomain::Sql, 0)],
        seed_state: ImpactSeedState::Resolved,
        seeds: vec![node(ImpactDomain::Css, "class", "card", Some("src/card.css"), Some(3))],
        direction: ImpactDirection::Dependents,
        depth: 3,
        strength_filter: backend_library::impact::ImpactStrengthFilter::Any,
        totals: ImpactTotals { exact, candidate, files: 2, by_kind: vec![("component".into(), rows.len() as u32)] },
        rows,
        gaps: Vec::new(),
        freshness: ImpactFreshness::Current { checked: 6 },
        terminal: ImpactTerminal::Complete,
        next: Vec::new(),
    }
}

#[test]
fn an_exact_row_and_a_possible_row_are_never_mixed_and_the_gaps_stay_beside_them() {
    let mut answer = report(vec![
        row(1, 0, ImpactStrength::Exact, node(ImpactDomain::Code, "component", "Card", Some("src/Card.tsx"), Some(7)), "code.uses-module-class", "imports Card.module.css"),
        row(2, 1, ImpactStrength::Candidate, node(ImpactDomain::Code, "component", "Page", Some("src/Page.tsx"), Some(2)), "code.renders", "renders Card"),
    ]);
    answer.gaps.push(ImpactGap { facet: "code.dynamic-class".into(), note: "class names built at run time".into(), count: Some(3) });
    let route = ImpactRoute::new(subject(NativeKind::CssClass, "card"));
    let model = impact_model(&route, &answer, &answer.rows);
    assert_eq!(model.exact, 1);
    assert_eq!(model.possible, 1);
    assert_eq!(model.rows[0].strength, Strength::Exact);
    assert_eq!(model.rows[1].strength, Strength::Possible);
    assert_eq!(model.rows[1].via, Some(1), "a possible row names the row it was reached through");
    assert_eq!(model.rows[0].via, None, "an exact row straight from the seed names none");
    assert_eq!(model.rows[0].site.as_ref().map(Site::label).as_deref(), Some("src/Card.tsx:7"));
    assert_eq!(model.rows[0].follow.as_ref().map(|follow| follow.kind), Some(NativeKind::CodeComponent));
    assert_eq!(model.gaps.len(), 1);
    assert!(model.gaps[0].sentence().contains("code.dynamic-class (3)"));
    assert!(!model.proves_no_impact, "rows were found");
    assert!(matches!(model.verdict(), Verdict::Affects { exact: 1, possible: 1, files: 2 }));
}

#[test]
fn zero_rows_prove_nothing_unless_the_owner_says_every_language_was_analysed_without_a_gap() {
    let route = ImpactRoute::new(subject(NativeKind::SqlTable, "orders"));
    let clean = report(Vec::new());
    assert_eq!(impact_model(&route, &clean, &[]).verdict(), Verdict::ProvedNone);

    let mut gapped = report(Vec::new());
    gapped.gaps.push(ImpactGap { facet: "sql.unparsed".into(), note: "one file did not parse".into(), count: Some(1) });
    assert!(matches!(impact_model(&route, &gapped, &[]).verdict(), Verdict::UnprovenNone(_)));

    let mut partial = report(Vec::new());
    partial.domains[1].state = ImpactDomainState::Partial { reasons: vec!["unparsed files".into()] };
    assert!(matches!(impact_model(&route, &partial, &[]).verdict(), Verdict::UnprovenNone(_)));

    let mut stale = report(Vec::new());
    stale.freshness = ImpactFreshness::Stale { checked: 3, changed: vec!["db/001.sql".into()], action: "re-index the project, then ask again".into() };
    let model = impact_model(&route, &stale, &[]);
    assert!(model.is_stale());
    assert!(matches!(model.verdict(), Verdict::UnprovenNone(_)));

    let mut budget = report(Vec::new());
    budget.terminal = ImpactTerminal::BudgetLimited { budget: "rows".into() };
    let model = impact_model(&route, &budget, &[]);
    assert_eq!(model.end, ListEnd::BudgetLimited("rows".into()));
    assert!(matches!(model.verdict(), Verdict::UnprovenNone(_)));
}

#[test]
fn a_name_the_owner_could_not_resolve_carries_its_reason_and_no_rows() {
    let route = ImpactRoute::new(subject(NativeKind::SqlTable, "orders"));
    let mut unavailable = report(Vec::new());
    unavailable.seed_state = ImpactSeedState::Unavailable { why: "no SQL worker".into() };
    unavailable.seeds.clear();
    let model = impact_model(&route, &unavailable, &[]);
    assert!(matches!(model.verdict(), Verdict::NothingSearched(why) if &*why == "no SQL worker"));

    let mut missing = report(Vec::new());
    missing.seed_state = ImpactSeedState::NotFound { why: "The analysed scope holds no such name.".into() };
    assert!(matches!(impact_model(&route, &missing, &[]).verdict(), Verdict::NameNotFound(_)));

    let mut ambiguous = report(Vec::new());
    ambiguous.seed_state = ImpactSeedState::Ambiguous;
    ambiguous.seeds = vec![
        node(ImpactDomain::Css, "class", "card", Some("src/a.css"), Some(1)),
        node(ImpactDomain::Css, "class", "card", Some("src/b.css"), Some(9)),
    ];
    let model = impact_model(&route, &ambiguous, &[]);
    assert_eq!(model.verdict(), Verdict::Ambiguous);
    let paths: Vec<_> = model.seeds.iter().filter_map(|seed| seed.follow.as_ref()?.path.clone()).collect();
    assert_eq!(paths, vec![Arc::<str>::from("src/a.css"), Arc::<str>::from("src/b.css")], "each candidate can be followed by its file");
}

#[test]
fn pages_follow_the_owners_cursor_and_the_view_says_when_it_is_full() {
    let secret = [7u8; 32];
    let cursor = ImpactCursor::issue(ImpactCursorClaims { manifest_root: [1; 32], recipe: [2; 32], offset: 1 }, &secret).expect("cursor");
    let first = {
        let mut page = report(vec![row(1, 0, ImpactStrength::Exact, node(ImpactDomain::Code, "component", "A", Some("a.tsx"), Some(1)), "r", "w")]);
        page.totals.exact = 2;
        page.terminal = ImpactTerminal::More { cursor: cursor.clone() };
        page
    };
    let second = {
        let mut page = report(vec![row(2, 0, ImpactStrength::Exact, node(ImpactDomain::Code, "component", "B", Some("b.tsx"), Some(1)), "r", "w")]);
        page.totals.exact = 2;
        page
    };
    let mut pages = vec![second.clone(), first.clone()];
    let mut seen_cursors = Vec::new();
    let mut engine = Scripted::new(move |command| {
        let SurfaceCommand::Impact { request } = command else { panic!("unexpected {command:?}") };
        seen_cursors.push(request.continuation.is_some());
        Ok(SurfaceReply::Impact(pages.pop().expect("page")))
    });
    let route = ImpactRoute::new(subject(NativeKind::CssClass, "card")).more().expect("two pages");
    let model = impact(&mut engine, &route, &CancellationToken::new()).expect("model");
    assert_eq!(model.rows.iter().map(|row| &*row.name).collect::<Vec<_>>(), ["A", "B"]);
    assert_eq!(model.end, ListEnd::Complete);
    assert_eq!(engine.asked, ["impact", "impact"]);

    // One page held, more to come: the view offers another page; at the cap it says it is full.
    let model = impact_model(&ImpactRoute::new(subject(NativeKind::CssClass, "card")), &first, &first.rows);
    assert_eq!(model.end, ListEnd::More);
    assert!(!model.proves_no_impact, "an unfinished list proves nothing");
    let mut full = ImpactRoute::new(subject(NativeKind::CssClass, "card"));
    while let Some(next) = full.more() {
        full = next;
    }
    assert_eq!(impact_model(&full, &first, &first.rows).end, ListEnd::ViewFull);
}

#[test]
fn a_cancelled_read_stops_between_owner_round_trips() {
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let mut engine = Scripted::new(move |_| {
        stop.cancel();
        Ok(SurfaceReply::Impact(report(Vec::new())))
    });
    let route = ImpactRoute::new(subject(NativeKind::CssClass, "card")).more().expect("more");
    let _ = route;
    let route = ImpactRoute { subject: subject(NativeKind::CssClass, "card"), pages: 3 };
    // The first answer is complete, so the read ends there and never asks again.
    let model = impact(&mut engine, &route, &cancel).expect("one answer");
    assert_eq!(engine.asked.len(), 1);
    assert!(model.rows.is_empty());
    let mut engine = Scripted::new(|_| panic!("a cancelled read must not ask the owner"));
    assert!(matches!(impact(&mut engine, &route, &cancel), Err(ReadFailure::Cancelled)));
}

// ---------------------------------------------------------------------------
// SQL
// ---------------------------------------------------------------------------

fn sql_source(n: u8) -> SqlSourceReference {
    SqlSourceReference::issue(SqlSourceClaims { manifest_root: [1; 32], sql_root: [2; 32], project: [3; 32], host: [4; 32], fact: [n; 32] }, &[9; 32]).expect("reference")
}

fn sql_basis(attempt: SqlAttemptState) -> SqlQueryBasis {
    SqlQueryBasis {
        manifest_root: [1; 32],
        sql_root: Some([2; 32]),
        project: [3; 32],
        attempt,
        host_health: SqlHostHealth::Unknown,
        inventory_complete: true,
        policy_complete: true,
        source_occurrences_complete: true,
        gaps: SqlGapCounts::default(),
        implicit_selection: true,
        catalog_is_deployed: false,
    }
}

fn sql_hit(n: u8, path: &str, fact: SqlFact, source: bool) -> SqlQueryHit {
    SqlQueryHit {
        id: [n; 32],
        path: path.to_owned(),
        dialect: SqlDialect::Sqlite,
        policy: SqlSourcePolicy::Unknown,
        host_basis: SqlHostBasis {
            input: [4; 32],
            source: [5; 32],
            length: 100,
            producer: [6; 32],
            settings: [7; 32],
            proposed_context: None,
            terminal: SqlNativeTerminal::Parsed { source: SqlCoverage::Partial, contextual: SqlCoverage::Partial, sqlite_lineage: None },
        },
        fact,
        source: source.then(|| sql_source(n)),
    }
}

fn definition(kind: SqlObjectKind, action: SqlDefinitionAction, name: &str, parent: Option<&str>, live: bool, path: &str) -> SqlFact {
    SqlFact::Definition(Box::new(SqlDefinition {
        kind,
        action,
        conditional: false,
        schema: None,
        name: name.to_owned(),
        parent: parent.map(str::to_owned),
        parent_schema: None,
        name_extent: SqlByteRange { start: 0, end: 1 },
        extent: SqlByteRange { start: 0, end: 10 },
        detail: None,
        statement: 0,
        entity: Some(SqlEntityRef { path: path.to_owned(), ordinal: 1 }),
        parent_entity: None,
        live_at_head: live,
        head_name: Some(name.to_owned()),
        head_parent: parent.map(str::to_owned),
    }))
}

fn reference(kind: SqlRefKind, role: SqlRefRole, name: &str, statement: SqlStatementKind, live: bool) -> SqlFact {
    SqlFact::Reference(Box::new(SqlReference {
        kind,
        role,
        schema: None,
        qualifier: None,
        name: name.to_owned(),
        extent: SqlByteRange { start: 0, end: 1 },
        full_extent: SqlByteRange { start: 0, end: 5 },
        statement: 0,
        statement_kind: statement,
        in_object: None,
        resolution: SqlResolution::Resolved { target: SqlEntityRef { path: "db/001.sql".into(), ordinal: 1 }, live },
    }))
}

fn sql_page_of(basis: SqlQueryBasis, hits: Vec<SqlQueryHit>) -> SqlQueryPage {
    SqlQueryPage { basis, hits, terminal: SqlQueryTerminal::Complete }
}

struct Library {
    basis: SqlQueryBasis,
    /// What `SqlReadSource` says for the fact `n`: (line, text), or a changed file.
    changed: Vec<u8>,
}

impl Library {
    fn answer(&self, command: &SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        match command {
            SurfaceCommand::SqlQuery { request } => {
                let hits = match (request.role, request.name.as_deref(), request.parent.as_deref(), request.uses.as_deref()) {
                    (SqlQueryRole::Definition, Some("orders"), _, _) => vec![
                        sql_hit(1, "db/001_init.sql", definition(SqlObjectKind::Table, SqlDefinitionAction::Create, "orders", None, true, "db/001_init.sql"), true),
                        sql_hit(2, "db/004_rename.sql", definition(SqlObjectKind::Table, SqlDefinitionAction::Alter, "orders", None, true, "db/004_rename.sql"), true),
                    ],
                    (SqlQueryRole::Definition, None, Some("orders"), _) => vec![
                        sql_hit(3, "db/001_init.sql", definition(SqlObjectKind::Column, SqlDefinitionAction::Create, "total", Some("orders"), true, "db/001_init.sql"), true),
                        sql_hit(4, "db/003_drop.sql", definition(SqlObjectKind::Column, SqlDefinitionAction::DropColumn, "note", Some("orders"), false, "db/003_drop.sql"), true),
                    ],
                    (SqlQueryRole::Reference, _, _, Some("orders")) => vec![
                        sql_hit(5, "queries/recent.sql", reference(SqlRefKind::Relation, SqlRefRole::Read, "orders", SqlStatementKind::Select, true), true),
                        sql_hit(6, "src/repo.ts", reference(SqlRefKind::Column, SqlRefRole::Write, "total", SqlStatementKind::Update, false), true),
                    ],
                    (SqlQueryRole::Gap, ..) => vec![sql_hit(
                        7,
                        "db/009_fn.sql",
                        SqlFact::Statement {
                            class: SqlStatementKind::CreateFunction,
                            extent: SqlByteRange { start: 0, end: 4 },
                            support: SqlStatementSupport::Partial(backend_semantic::sql_analysis::SqlGap::OpaqueBody),
                        },
                        false,
                    )],
                    _ => Vec::new(),
                };
                Ok(SurfaceReply::SqlQuery(sql_page_of(self.basis.clone(), hits)))
            }
            SurfaceCommand::SqlReadSource { reference } => {
                // The source reference is opaque; the fixture reads the fact number from it by claim order.
                let claims = reference.verify(&[9; 32]).expect("reference");
                let n = claims.fact[0];
                if self.changed.contains(&n) {
                    return Ok(SurfaceReply::SqlReadSource(SqlSourceRead::SourceChanged));
                }
                Ok(SurfaceReply::SqlReadSource(SqlSourceRead::Available {
                    reference: reference.clone(),
                    policy: SqlSourcePolicy::Unknown,
                    served_root: "/workspace/shop".into(),
                    path: "x".into(),
                    start: 0,
                    end: 1,
                    line_1: u32::from(n) * 10,
                    text: "orders".into(),
                }))
            }
            SurfaceCommand::Impact { .. } => {
                let mut answer = report(vec![row(1, 0, ImpactStrength::Exact, node(ImpactDomain::Code, "component", "OrderList", Some("src/OrderList.tsx"), Some(4)), "code.queries-table", "runs a query on orders")]);
                answer.seeds = vec![node(ImpactDomain::Sql, "table", "orders", Some("db/001_init.sql"), Some(10))];
                answer.gaps.push(ImpactGap { facet: "code.dynamic-sql".into(), note: "SQL built from strings".into(), count: None });
                Ok(SurfaceReply::Impact(answer))
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
}

#[test]
fn a_table_page_reads_definition_columns_migrations_uses_and_what_the_analysis_could_not_read() {
    let library = Library { basis: sql_basis(SqlAttemptState::LatestObserved), changed: Vec::new() };
    let mut engine = Scripted::new(move |command| library.answer(command));
    let model = sql(&mut engine, &subject(NativeKind::SqlTable, "orders"), &QueryTimeLinks, &CancellationToken::new()).expect("model");

    assert!(matches!(&model.object, SqlObject::Defined { kind: NativeKind::SqlTable, live: true, .. }));
    assert_eq!(model.definitions.len(), 2);
    assert_eq!(&*model.definitions[0].action, "created");
    assert_eq!(model.definitions[0].site.label(), "db/001_init.sql:10", "the line comes from the owner's re-read, not a guess");
    assert_eq!(model.definitions[0].verified, Verified::Matches);
    assert_eq!(&*model.definitions[1].action, "altered");

    assert_eq!(model.columns.len(), 2);
    let note = model.columns.iter().find(|column| &*column.name == "note").expect("dropped column");
    assert!(!note.live, "a dropped column is listed and said to be dropped");
    assert_eq!(&*note.sites[0].action, "dropped as a column");

    assert_eq!(model.uses.len(), 2);
    assert!(!model.uses[0].in_code && &*model.uses[0].statement == "SELECT" && &*model.uses[0].role == "reads");
    assert!(model.uses[1].in_code, "a mention in a .ts file is a use in code");
    assert!(!model.uses[1].live, "a mention bound to a dropped definition is kept and said to be");
    assert_eq!(model.uses[1].column.as_deref(), Some("total"));

    assert_eq!(model.gap_sites.len(), 1);
    assert!(model.gap_sites[0].why.contains("CREATE FUNCTION") && model.gap_sites[0].why.contains("opaque"));
    assert!(model.users.rows.iter().all(|row| row.domain == NativeDomain::Code));
    assert_eq!(model.users.rows.len(), 1);
    assert_eq!(model.users.gaps.len(), 1, "what the use analysis cannot see is kept with the users");
    assert!(model.coverage.analysed && model.coverage.complete);
    assert!(model.coverage.notes.iter().any(|note| note.contains("Nothing was read from a running database")));
    assert!(model.changed.is_empty());
}

#[test]
fn a_file_that_changed_since_indexing_is_named_and_its_rows_are_not_called_verified() {
    let library = Library { basis: sql_basis(SqlAttemptState::LatestObserved), changed: vec![5] };
    let mut engine = Scripted::new(move |command| library.answer(command));
    let model = sql(&mut engine, &subject(NativeKind::SqlTable, "orders"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert_eq!(model.changed, vec![Arc::<str>::from("queries/recent.sql")]);
    let changed = model.uses.iter().find(|usage| &*usage.site.path == "queries/recent.sql").expect("use");
    assert_eq!(changed.verified, Verified::Changed);
    assert_eq!(changed.site.line, None, "a line the owner could not re-read is not invented");
}

#[test]
fn partial_and_missing_analysis_says_so_and_stops_asking() {
    let mut basis = sql_basis(SqlAttemptState::LatestObserved);
    basis.inventory_complete = false;
    basis.gaps = SqlGapCounts { unavailable_hosts: 2, native_error_hosts: 1, partial_statements: 3, unsupported_statements: 0 };
    let coverage = sql_coverage(&sql_page_of(basis, Vec::new()));
    assert!(coverage.analysed && !coverage.complete);
    let all = coverage.notes.join(" | ");
    for expected in ["Not every SQL file", "2 SQL files could not be read", "1 SQL file was rejected", "3 statements were only partly understood"] {
        assert!(all.contains(expected), "{expected:?} missing from {all}");
    }
    assert!(!all.contains("not understood at all"), "a zero count is not a sentence");

    // Nothing analysed: the page is unavailable, never "not defined", and asks nothing more.
    let library = Library { basis: sql_basis(SqlAttemptState::NoSelectedAnalysis), changed: Vec::new() };
    let mut engine = Scripted::new(move |command| library.answer(command));
    let model = sql(&mut engine, &subject(NativeKind::SqlTable, "orders"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert!(matches!(model.object, SqlObject::Unavailable(_)));
    assert!(!model.coverage.analysed);
    assert!(model.uses.is_empty() && model.columns.is_empty());
    assert!(matches!(model.users.state, UsersState::Unavailable(_)));
    assert_eq!(engine.asked, ["sql-query Definition"], "an unanalysed project is asked once");

    // Analysed, but the files that might define it could not be read: could not look, not "not there".
    let mut engine = Scripted::new(|command| match command {
        SurfaceCommand::SqlQuery { .. } => {
            let mut basis = sql_basis(SqlAttemptState::LatestObserved);
            basis.gaps = SqlGapCounts { unavailable_hosts: 2, ..SqlGapCounts::default() };
            Ok(SurfaceReply::SqlQuery(sql_page_of(basis, Vec::new())))
        }
        other => panic!("unexpected {other:?}"),
    });
    let model = sql(&mut engine, &subject(NativeKind::SqlTable, "orders"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert!(matches!(&model.object, SqlObject::Unavailable(why) if why.contains("2 SQL files could not be read")), "{:?}", model.object);
    assert!(!model.coverage.analysed);

    // Analysed, and the name is not there: not defined, in words that are not proof.
    let mut engine = Scripted::new(|command| match command {
        SurfaceCommand::SqlQuery { .. } => Ok(SurfaceReply::SqlQuery(sql_page_of(sql_basis(SqlAttemptState::LatestObserved), Vec::new()))),
        other => panic!("unexpected {other:?}"),
    });
    let model = sql(&mut engine, &subject(NativeKind::SqlTable, "ghost"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert_eq!(model.object, SqlObject::NotDefined);
}

#[test]
fn a_dialect_that_folds_case_is_retried_in_lower_case() {
    let library = Library { basis: sql_basis(SqlAttemptState::LatestObserved), changed: Vec::new() };
    let mut engine = Scripted::new(move |command| library.answer(command));
    let model = sql(&mut engine, &subject(NativeKind::SqlTable, "Orders"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert!(matches!(model.object, SqlObject::Defined { .. }), "Orders finds orders");
}

#[test]
fn a_column_page_is_its_tables_page_with_the_column_marked() {
    let library = Library { basis: sql_basis(SqlAttemptState::LatestObserved), changed: Vec::new() };
    let mut engine = Scripted::new(move |command| library.answer(command));
    let model = sql(&mut engine, &subject(NativeKind::SqlColumn, "orders.total"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert_eq!(&*model.object_name, "orders");
    assert_eq!(model.column.as_deref(), Some("total"));
    assert!(matches!(model.object, SqlObject::Defined { .. }));
}

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

fn stylo_source(n: u8) -> StyloSourceReference {
    StyloSourceReference::issue(StyloSourceClaims { manifest_root: [1; 32], native_root: [2; 32], project: [3; 32], host: [4; 32], site: [n; 32] }, &[9; 32]).expect("reference")
}

fn stylo_basis(attempt: StyloAttemptState) -> StyloQueryBasis {
    StyloQueryBasis {
        manifest_root: [1; 32],
        native_root: Some([2; 32]),
        attempt,
        worker_capability: StyloWorkerCapability::Unknown,
        host_terminal_summary: StyloHostTerminalSummary::Unknown,
        source_partial: false,
        gaps: vec![StyloScopeGap::DomAndCascadeNotAttempted, StyloScopeGap::ImportsNotResolved],
        producer: None,
        selection_complete: true,
    }
}

fn stylo_rule(line: u32, condition: Option<&str>) -> StyloRule {
    StyloRule {
        path: vec![0],
        kind: StyloRuleKind::Style,
        native_kind: "Style".into(),
        condition: condition.map(str::to_owned),
        location: Some(StyloLocation { line, column: 1, approximate: true }),
    }
}

fn stylo_selector(canonical: &str) -> StyloSelector {
    StyloSelector {
        rule_path: vec![0],
        ordinal: 0,
        canonical: canonical.to_owned(),
        specificity: 1,
        components: vec![StyloSelectorComponent { kind: StyloSelectorComponentKind::Class, name: "card".into() }],
        component_coverage: StyloDependencyCoverage::Known,
    }
}

fn site(role: StyloSourceRole, raw: &str) -> StyloSourceOccurrence {
    StyloSourceOccurrence {
        rule_path: vec![0],
        selector_ordinal: Some(0),
        role,
        start: 0,
        end: raw.len() as u64,
        raw: raw.to_owned(),
        decoded: raw.trim_start_matches('.').to_owned(),
        disposition: StyloSourceDisposition::Unknown,
    }
}

fn stylo_page_of(basis: StyloQueryBasis, hits: Vec<StyloQueryHit>) -> StyloQueryPage {
    StyloQueryPage { basis, hits, terminal: StyloQueryTerminal::Complete }
}

fn css_script(basis: StyloQueryBasis, hits: Vec<StyloQueryHit>, changed: bool) -> impl FnMut(&SurfaceCommand) -> Result<SurfaceReply, ClientError> {
    move |command| match command {
        SurfaceCommand::StyloQuery { .. } => Ok(SurfaceReply::StyloQuery(stylo_page_of(basis.clone(), hits.clone()))),
        SurfaceCommand::StyloReadSite { .. } if changed => Ok(SurfaceReply::StyloReadSite(StyloSourceRead::SourceChanged)),
        SurfaceCommand::StyloReadSite { .. } => Ok(SurfaceReply::StyloReadSite(StyloSourceRead::Available {
            served_root: "/workspace/shop".into(),
            path: "src/card.css".into(),
            start: 0,
            end: 5,
            line_1: 12,
            text: ".card".into(),
        })),
        SurfaceCommand::Impact { .. } => {
            let mut answer = report(vec![
                row(1, 0, ImpactStrength::Exact, node(ImpactDomain::Code, "component", "Card", Some("src/Card.tsx"), Some(3)), "code.uses-module-class", "imports the module"),
                row(2, 0, ImpactStrength::Candidate, node(ImpactDomain::Code, "component", "Tile", Some("src/Tile.tsx"), Some(5)), "code.uses-class", "a literal className token"),
                row(3, 0, ImpactStrength::Exact, node(ImpactDomain::Css, "rule", ".card", Some("src/card.css"), Some(1)), "css.styles-class", "a rule"),
            ]);
            answer.gaps.push(ImpactGap { facet: "code.dynamic-class".into(), note: "class names built at run time".into(), count: Some(2) });
            Ok(SurfaceReply::Impact(answer))
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_class_page_names_its_rules_with_the_exact_source_the_owner_re_read() {
    let hits = vec![
        StyloQueryHit::SourceSite {
            id: [1; 32],
            path: "src/card.css".into(),
            site: site(StyloSourceRole::ClassName, ".card"),
            rule: stylo_rule(3, Some("(min-width: 40em)")),
            selector: Some(stylo_selector(".card")),
            source: stylo_source(1),
            source_gaps: Vec::new(),
        },
        // The same selector, natively: it must not appear twice.
        StyloQueryHit::NativeSelector { id: [2; 32], path: "src/card.css".into(), rule: stylo_rule(3, None), selector: stylo_selector(".card") },
        // A different rule the observer did not place a site for: kept, and not called verified.
        StyloQueryHit::NativeSelector { id: [3; 32], path: "src/other.css".into(), rule: stylo_rule(0, None), selector: StyloSelector { rule_path: vec![1], ordinal: 0, ..stylo_selector(".card:hover") } },
    ];
    let mut engine = Scripted::new(css_script(stylo_basis(StyloAttemptState::LatestObserved), hits, false));
    let model = css(&mut engine, &subject(NativeKind::CssClass, "card"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert_eq!(model.state, CssState::Found);
    assert_eq!(model.rules.len(), 2);
    let authored = &model.rules[0];
    assert_eq!(authored.site.label(), "src/card.css:12", "the exact line is the owner's re-read, not the parser's approximate one");
    assert!(!authored.approximate);
    assert_eq!(authored.written.as_deref(), Some(".card"));
    assert_eq!(authored.verified, Verified::Matches);
    assert_eq!(authored.condition.as_deref(), Some("(min-width: 40em)"));
    let native = &model.rules[1];
    assert_eq!(native.verified, Verified::Unread);
    assert!(native.approximate && native.site.line == Some(1));
    assert_eq!(&*native.selector, ".card:hover");

    // Components come through the usage interface, kept to code, strength intact.
    assert_eq!(model.users.rows.len(), 2);
    assert_eq!((model.users.exact, model.users.possible), (1, 1));
    assert!(model.users.gaps.iter().any(|gap| &*gap.facet == "code.dynamic-class"));
    // The owner's declared blind spots are said, not dropped.
    assert!(!model.coverage.complete, "a declared gap means the coverage is not complete");
    let notes = model.coverage.notes.join(" | ");
    assert!(notes.contains("cascade") && notes.contains("@import"), "{notes}");
}

#[test]
fn a_changed_stylesheet_is_named_and_a_missing_analysis_never_reads_as_not_found() {
    let hits = vec![StyloQueryHit::SourceSite {
        id: [1; 32],
        path: "src/card.css".into(),
        site: site(StyloSourceRole::ClassName, ".card"),
        rule: stylo_rule(3, None),
        selector: Some(stylo_selector(".card")),
        source: stylo_source(1),
        source_gaps: Vec::new(),
    }];
    let mut engine = Scripted::new(css_script(stylo_basis(StyloAttemptState::LatestObserved), hits, true));
    let model = css(&mut engine, &subject(NativeKind::CssClass, "card"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert_eq!(model.changed, vec![Arc::<str>::from("src/card.css")]);
    assert_eq!(model.rules[0].verified, Verified::Changed);

    let mut engine = Scripted::new(css_script(stylo_basis(StyloAttemptState::NoSelectedAnalysis), Vec::new(), false));
    let model = css(&mut engine, &subject(NativeKind::CssClass, "card"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert!(matches!(model.state, CssState::Unavailable(_)), "no analysis is unavailable, not not-found");
    assert!(matches!(model.users.state, UsersState::Unavailable(_)));
    assert_eq!(engine.asked, ["stylo-query Class"]);

    let mut engine = Scripted::new(css_script(stylo_basis(StyloAttemptState::LatestObserved), Vec::new(), false));
    let model = css(&mut engine, &subject(NativeKind::CssClass, "ghost"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert_eq!(model.state, CssState::NotFound);

    let mut engine = Scripted::new(css_script(stylo_basis(StyloAttemptState::LatestObserved), Vec::new(), false));
    assert!(css(&mut engine, &subject(NativeKind::SqlTable, "orders"), &QueryTimeLinks, &CancellationToken::new()).is_err(), "the CSS page reads classes and custom properties only");
}

#[test]
fn a_custom_property_page_separates_where_it_is_declared_from_what_reads_it() {
    let declaration = StyloDeclaration {
        rule_path: vec![0],
        ordinal: 0,
        kind: StyloDeclarationKind::Custom,
        property: "--brand".into(),
        value: "#336699".into(),
        value_available: true,
        important: false,
        dependency_coverage: StyloDependencyCoverage::Known,
    };
    let reader = StyloDeclaration { property: "color".into(), ordinal: 1, kind: StyloDeclarationKind::WithVariables, value: String::new(), value_available: false, ..declaration.clone() };
    let hits = vec![
        StyloQueryHit::SourceSite {
            id: [1; 32],
            path: "src/theme.css".into(),
            site: StyloSourceOccurrence { selector_ordinal: None, role: StyloSourceRole::CustomPropertyDeclarationName, ..site(StyloSourceRole::CustomPropertyDeclarationName, "--brand") },
            rule: stylo_rule(1, None),
            selector: None,
            source: stylo_source(1),
            source_gaps: Vec::new(),
        },
        StyloQueryHit::NativeDeclaration { id: [2; 32], path: "src/theme.css".into(), rule: stylo_rule(1, None), declaration: declaration.clone() },
        StyloQueryHit::NativeDependency {
            id: [3; 32],
            path: "src/card.css".into(),
            rule: stylo_rule(4, None),
            declaration: reader,
            dependency: StyloVariableDependency { rule_path: vec![0], declaration_ordinal: 1, name: "--brand".into(), kind: "var".into(), reference_path: vec![0] },
        },
    ];
    let mut engine = Scripted::new(css_script(stylo_basis(StyloAttemptState::LatestObserved), hits, false));
    let model = css(&mut engine, &subject(NativeKind::CssCustomProperty, "--brand"), &QueryTimeLinks, &CancellationToken::new()).expect("model");
    assert_eq!(model.declarations.len(), 1, "the authored site and its native value are one declaration");
    assert_eq!(model.declarations[0].value.as_deref(), Some("#336699"));
    assert_eq!(model.declarations[0].verified, Verified::Matches);
    assert_eq!(model.reads.len(), 1);
    assert_eq!((&*model.reads[0].property, &*model.reads[0].how), ("color", "var"));
    assert_eq!(model.reads[0].site.label(), "src/card.css:5");
}

// ---------------------------------------------------------------------------
// Find
// ---------------------------------------------------------------------------

#[test]
fn find_looks_in_sql_and_css_only_for_a_query_long_enough_to_mean_something() {
    let mut engine = Scripted::new(|command| panic!("a one-letter query asked {command:?}"));
    let find = find_native(&mut engine, &SearchQuery::new("a", 50).expect("query"), &CancellationToken::new()).expect("find");
    assert!(!find.asked);
    assert!(find.hits.is_empty());

    // The index cannot list its projects: nothing was searched, and Find says so by `searched`.
    let mut engine = Scripted::new(|command| panic!("no project, no asking: {command:?}"));
    let find = find_native(&mut engine, &SearchQuery::new("orders", 50).expect("query"), &CancellationToken::new()).expect("find");
    assert!(find.asked);
    assert_eq!(find.searched, 0);
}

#[test]
fn names_are_checked_before_they_are_sent_to_the_owner() {
    assert!(is_sql_name("orders") && is_sql_name("orders.total") && is_sql_name("public_orders"));
    assert!(!is_sql_name("a.b.c") && !is_sql_name(".a") && !is_sql_name("a b") && !is_sql_name(""));
    assert!(is_css_name("card-title") && is_css_name("--brand"));
    assert!(!is_css_name("a b") && !is_css_name("a>b") && !is_css_name(""));
    assert_eq!(join_names(&["a".into(), "b".into(), "c".into()]), "a, b, and c");
    assert_eq!(join_names(&["a".into(), "b".into()]), "a and b");
}
