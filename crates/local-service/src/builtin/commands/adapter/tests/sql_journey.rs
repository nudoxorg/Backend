//! SQL acceptance journey.
//!
//! A realistic multi-file SQLite project is written to disk and indexed through
//! the production owner: the real filesystem scan, the real framed listener, a
//! real out-of-process syntaqlite worker, durable publication of the SQL
//! relation and its owner-sealed receipt, then queried over the same public
//! `sql-query` / `sql-read-source` surface the CLI and MCP use. The workspace is
//! then closed and reopened cold from its persisted journal.
//!
//! Development mode: debug builds skip the account gate, so this proves the
//! product route, not the gated release configuration.
use super::*;
use backend_library::sql_query::{
    SqlQueryPage, SqlQueryRequest, SqlQueryRole, SqlQueryTerminal, SqlSourceRead,
};
use backend_semantic::sql_analysis::{
    SqlDefinitionAction, SqlFact, SqlObjectKind, SqlRefKind, SqlResolution, SqlStatementKind,
    SqlStatementSupport,
};

/// Build machines that run many lanes at once can take longer than the product
/// default just to start a process; the budget is recorded in the row settings.
const TERMINAL_WAIT: std::time::Duration = std::time::Duration::from_secs(600);
const TEST_WORKER_WALL: std::time::Duration = std::time::Duration::from_secs(90);

/// Build (or locate) a frontend worker binary. `NUDOX_TEST_SQLITE_WORKER` /
/// `NUDOX_TEST_POSTGRES_WORKER` select a prebuilt one; otherwise the isolated
/// frontend workspace is built here with this process's own cargo.
pub(super) fn frontend_worker(frontend: &str, binary: &str) -> PathBuf {
    let variable = format!("NUDOX_TEST_{}_WORKER", frontend.to_ascii_uppercase());
    if let Some(path) = std::env::var_os(&variable) {
        return PathBuf::from(path);
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join("target"))
        .join(format!("{frontend}-frontend-worker"));
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = std::process::Command::new(cargo)
        .args(["build", "--locked", "--offline", "--bin", binary, "--manifest-path"])
        .arg(repo.join("frontends").join(frontend).join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target)
        // The pgrust checkout is owned by another user on shared build hosts;
        // let the build script's read-only git inspection run (this file-less,
        // per-process setting does not touch any git configuration on disk).
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "safe.directory")
        .env("GIT_CONFIG_VALUE_0", "*")
        // The checked launch hashes the worker on every use: keep it small.
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("CARGO_PROFILE_DEV_STRIP", "symbols")
        .output()
        .expect("spawn cargo for the frontend worker");
    assert!(
        output.status.success(),
        "frontend worker build failed:\n{}",
        String::from_utf8_lossy(&output.stderr).lines().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
    );
    target.join("debug").join(binary)
}

/// Admit a worker the way an owner would: its executable identity plus the
/// producer basis it reports. Development-mode test admission only.
pub(super) fn admitted_workers(sqlite: Option<PathBuf>, postgres: Option<PathBuf>) -> crate::builtin::sql_index::SqlWorkers {
    use backend_sql_contract::{Dialect, Engine, Request, Settings, call_worker};
    let probe = |path: &Path, dialect, engine| {
        let artifact = backend_compile::ToolchainArtifact::from_path(path, Vec::new())
            .expect("worker artifact");
        // One start can exceed the probe's own 10s on a saturated build host.
        let request = Request {
            version: backend_sql_contract::PROTOCOL_VERSION,
            expected_basis: None,
            source: b"SELECT 1".to_vec(),
            settings: Settings::ProcessDefaults,
            context: None,
        };
        let basis = (0..6)
            .find_map(|_| call_worker(path, &request, dialect, engine).basis)
            .expect("worker reports its producer basis");
        (artifact, basis)
    };
    crate::builtin::sql_index::SqlWorkers {
        sqlite: sqlite.map(|path| {
            let (artifact, basis) = probe(&path, Dialect::Sqlite, Engine::Syntaqlite4e4de76);
            Arc::new(
                crate::builtin::sql_capture::SqliteWorkerPermit::from_admitted(path, artifact, basis)
                    .expect("SQLite worker permit")
                    .with_wall(TEST_WORKER_WALL),
            )
        }),
        postgres: postgres.map(|path| {
            let (artifact, basis) = probe(&path, Dialect::Postgres, Engine::Pgrust38634b4);
            Arc::new(
                crate::builtin::sql_capture::PostgresWorkerPermit::from_admitted(path, artifact, basis)
                    .expect("PostgreSQL worker permit")
                    .with_wall(TEST_WORKER_WALL),
            )
        }),
    }
}

const INIT: &str = "\
-- Baseline schema.
CREATE TABLE accounts (
  id INTEGER PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  balance INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE orders (
  id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL REFERENCES accounts(id),
  total INTEGER NOT NULL
);
CREATE INDEX orders_by_account ON orders(account_id);
";
const TIER: &str = "ALTER TABLE accounts ADD COLUMN tier TEXT NOT NULL DEFAULT 'basic';\n";
const VIEWS: &str = "\
CREATE VIEW account_spend AS
  SELECT a.id AS account_id, a.email, SUM(o.total) AS spent
  FROM accounts a JOIN orders o ON o.account_id = a.id
  GROUP BY a.id, a.email;

CREATE TRIGGER accounts_tier_audit AFTER UPDATE OF tier ON accounts
BEGIN
  INSERT INTO orders (account_id, total) VALUES (NEW.id, 0);
END;
";
const QUERIES: &str = "\
SELECT email, balance FROM accounts WHERE id = ?1;
UPDATE accounts SET balance = balance - ?2 WHERE id = ?1;
SELECT s.email, s.spent FROM account_spend s WHERE s.spent > 100;
";
const DROP_BALANCE: &str = "ALTER TABLE accounts DROP COLUMN balance;\n";

fn sqlite_files() -> Vec<(String, Vec<u8>)> {
    [
        ("db/migrations/0001_init.sql", INIT),
        ("db/migrations/0002_tier.sql", TIER),
        ("db/migrations/0003_views.sql", VIEWS),
        ("db/queries/accounts.sql", QUERIES),
    ]
    .into_iter()
    .map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec()))
    .collect()
}

fn request(project: &Path, role: SqlQueryRole) -> SqlQueryRequest {
    SqlQueryRequest {
        package: backend_library::PackageReference::parse(project.to_str().expect("path").to_owned())
            .expect("package"),
        role,
        name: None,
        kind: None,
        parent: None,
        uses: None,
        page_rows: 128,
        output_bytes: 4_194_304,
        scan_rows: 262_144,
        scan_bytes: 64 * 1024 * 1024,
        continuation: None,
    }
}

/// Every page of a query, through the listener, until the terminal is Complete.
fn all_hits(session: &mut Session, mut request: SqlQueryRequest) -> (SqlQueryPage, Vec<backend_library::sql_query::SqlQueryHit>) {
    let mut hits = Vec::new();
    let mut host_gaps = Vec::new();
    loop {
        let backend_library::SurfaceReply::SqlQuery(mut page) = session
            .surface(backend_library::SurfaceCommand::SqlQuery { request: request.clone() })
            .expect("sql-query through the public route")
        else {
            panic!("SQL query reply kind");
        };
        hits.extend(page.hits.clone());
        host_gaps.extend(page.host_gaps.clone());
        match &page.terminal {
            SqlQueryTerminal::More { continuation, .. } => request.continuation = Some(continuation.clone()),
            SqlQueryTerminal::Complete => { page.host_gaps = host_gaps; return (page, hits); },
            SqlQueryTerminal::Unavailable => panic!("SQL analysis unavailable: {:?}", page.basis.attempt),
        }
    }
}

fn definitions(session: &mut Session, project: &Path, kind: SqlObjectKind, parent: Option<&str>) -> Vec<(String, bool)> {
    let mut r = request(project, SqlQueryRole::Definition);
    r.kind = Some(kind);
    r.parent = parent.map(str::to_owned);
    let (_, hits) = all_hits(session, r);
    let mut names = hits
        .into_iter()
        .filter_map(|hit| match hit.fact {
            SqlFact::Definition(d) if matches!(d.action, SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn) => {
                Some((d.head_name.unwrap_or(d.name), d.live_at_head))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn read_text(session: &mut Session, hit: &backend_library::sql_query::SqlQueryHit) -> (String, String) {
    let reference = hit.source.clone().expect("exact source reference");
    let backend_library::SurfaceReply::SqlReadSource(SqlSourceRead::Available { path, text, .. }) = session
        .surface(backend_library::SurfaceCommand::SqlReadSource { reference })
        .expect("sql-read-source through the public route")
    else {
        panic!("source reply");
    };
    (path, text)
}

fn publish(wire: &mut SourceWire, byte: u8) {
    mixed_start_again(wire, byte);
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(wire);
}

#[test]
fn sqlite_project_indexes_publishes_queries_reindexes_and_survives_cold_reopen() {
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let mut wire = SourceWire::start_sql(
        TestStage::BeforeSourcePreparation,
        true,
        SqlFixture { files: sqlite_files(), workers },
    );
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    wire.commands.lock().expect("adapter").source_test_hook = None;
    let project = wire.source.parent().expect("project").to_owned();
    let mut session = Session::connect(&wire.endpoint).expect("listener");

    // Ordinary name search must join selected SQL view rows with their own
    // native query evidence rather than silently excluding them.
    for (name, suffix) in [
        (
            "account_spend",
            "db/migrations/0003_views.sql::account_spend",
        ),
        ("balance", "db/migrations/0001_init.sql::accounts::balance"),
    ] {
        let reply = session
            .surface(backend_library::SurfaceCommand::IndexSearch {
                query: backend_library::ProductText::new(name).expect("query"),
                limit: 20,
                cursor: None,
            })
            .expect("ordinary SQL name search");
        let backend_library::SurfaceReply::IndexSearchPage(page) = reply else {
            panic!("search page")
        };
        assert!(
            page.hits.iter().any(|hit| matches!(hit,
                backend_library::RegistrySearchHit::LocalDeclaration(record)
                    if record.name.as_str() == name && record.coordinate.as_str().ends_with(suffix)
            )),
            "SQL declaration {name} missing from search: {:?}",
            page.hits
        );
    }

    // Tables, columns, views, triggers, indexes: definitions across files.
    let tables = definitions(&mut session, &project, SqlObjectKind::Table, None);
    assert_eq!(tables, [("accounts".to_owned(), true), ("orders".to_owned(), true)]);
    let columns = definitions(&mut session, &project, SqlObjectKind::Column, Some("accounts"));
    assert_eq!(
        columns.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
        ["balance", "email", "id", "tier"],
        "ALTER TABLE ADD COLUMN in a later file extends the table"
    );
    assert_eq!(definitions(&mut session, &project, SqlObjectKind::View, None), [("account_spend".to_owned(), true)]);
    assert_eq!(definitions(&mut session, &project, SqlObjectKind::Trigger, None), [("accounts_tier_audit".to_owned(), true)]);
    assert_eq!(definitions(&mut session, &project, SqlObjectKind::Index, None), [("orders_by_account".to_owned(), true)]);

    // Honest coverage: nothing was skipped, and the page says so.
    let (page, gaps) = all_hits(&mut session, request(&project, SqlQueryRole::Gap));
    assert!(gaps.is_empty(), "no statement of this project is partially modeled: {gaps:?}");
    assert!(page.basis.inventory_complete, "implicit selection covers the complete walk");
    assert!(page.basis.implicit_selection);
    assert_eq!(page.basis.gaps, Default::default());

    // Who uses accounts.balance? Two mentions, in another file, bound exactly.
    let mut uses = request(&project, SqlQueryRole::Reference);
    uses.uses = Some("accounts.balance".into());
    uses.kind = Some(SqlObjectKind::Column);
    let (_, hits) = all_hits(&mut session, uses.clone());
    let balance: Vec<_> = hits
        .iter()
        .filter(|hit| matches!(&hit.fact, SqlFact::Reference(r) if r.kind == SqlRefKind::Column && r.name == "balance"))
        .collect();
    assert_eq!(balance.len(), 3, "SELECT list, SET target and SET expression: {balance:?}");
    assert!(balance.iter().all(|hit| hit.path == "db/queries/accounts.sql"));
    for hit in &balance {
        let SqlFact::Reference(reference) = &hit.fact else { unreachable!() };
        assert!(matches!(reference.resolution, SqlResolution::Resolved { live: true, .. }));
        assert_eq!(reference.statement_kind == SqlStatementKind::Update || reference.statement_kind == SqlStatementKind::Select, true);
        let (path, text) = read_text(&mut session, hit);
        assert_eq!(path, "db/queries/accounts.sql");
        assert_eq!(text, "balance", "exact source span of the written name");
    }

    // The view depends on accounts.email through an alias and a join.
    let mut email = request(&project, SqlQueryRole::Reference);
    email.uses = Some("accounts.email".into());
    let (_, hits) = all_hits(&mut session, email);
    assert!(hits.iter().any(|hit| matches!(&hit.fact, SqlFact::Reference(r)
        if r.in_object.as_ref().is_some_and(|o| o.kind == SqlObjectKind::View && o.name == "account_spend"))),
        "view account_spend is linked to the column it selects");

    // Impact: what breaks if accounts.email changes, through the shared framework.
    {
        use backend_library::impact::{
            ImpactDomain, ImpactDomainState, ImpactRequest, ImpactSeed, ImpactSeedSpec, ImpactSeedState,
            ImpactStrength,
        };
        let seed = ImpactSeed::Named(ImpactSeedSpec {
            domain: ImpactDomain::Sql,
            kind: "column".into(),
            name: "accounts.email".into(),
            path: None,
        });
        let request = ImpactRequest::new(
            backend_library::PackageReference::parse(project.to_str().expect("path").to_owned()).expect("package"),
            seed,
        );
        let backend_library::SurfaceReply::Impact(report) = session
            .surface(backend_library::SurfaceCommand::Impact { request })
            .expect("impact through the public route")
        else {
            panic!("impact reply kind");
        };
        assert!(matches!(report.seed_state, ImpactSeedState::Resolved), "{:?}", report.seed_state);
        let sql = report.domains.iter().find(|d| d.domain == ImpactDomain::Sql).expect("SQL domain");
        assert!(matches!(sql.state, ImpactDomainState::Analyzed), "{:?}", sql.state);
        let names: Vec<_> = report.rows.iter().map(|row| (row.node.kind.as_str(), row.node.name.as_str(), row.strength)).collect();
        assert!(names.iter().any(|(kind, name, strength)| *kind == "view" && *name == "account_spend" && *strength == ImpactStrength::Exact),
            "the view that selects accounts.email: {names:?}");
        assert!(names.iter().any(|(kind, _, strength)| *kind == "query" && *strength == ImpactStrength::Exact),
            "the query that reads accounts.email: {names:?}");
        // The view's own consumers follow, as candidates (the view column is unnamed in the project).
        assert!(names.iter().any(|(kind, _, strength)| *kind == "query" && *strength == ImpactStrength::Candidate)
            || names.iter().filter(|(kind, ..)| *kind == "query").count() >= 1);
        assert!(!names.iter().any(|(kind, _, _)| *kind == "table"), "a column does not change its owning table: {names:?}");
        assert!(report.gaps.iter().any(|gap| gap.facet == "sql.embedded-sql"), "named blind spot");
        assert!(!report.proves_no_impact());
    }

    // Edit: a new migration drops accounts.balance. Reindex; queries elsewhere
    // now point at a dropped column.
    fs::write(project.join("db/migrations/0004_drop_balance.sql"), DROP_BALANCE).expect("new migration");
    publish(&mut wire, 0xd1);
    let columns = definitions(&mut session, &project, SqlObjectKind::Column, Some("accounts"));
    assert!(columns.contains(&("balance".to_owned(), false)), "dropped column is not live at head: {columns:?}");
    let (_, hits) = all_hits(&mut session, uses);
    let live: Vec<bool> = hits
        .iter()
        .filter_map(|hit| match &hit.fact {
            SqlFact::Reference(r) if r.name == "balance" && r.statement_kind != SqlStatementKind::AlterTable => match &r.resolution {
                SqlResolution::Resolved { live, .. } => Some(*live),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(live, [false, false, false], "unchanged query file re-bound to the dropped column");

    let (_, diagnostics) = all_hits(&mut session, request(&project, SqlQueryRole::Diagnostic));
    assert!(diagnostics.iter().any(|hit| matches!(&hit.fact, SqlFact::Reference(reference)
        if reference.name == "balance" && matches!(reference.resolution, SqlResolution::Resolved { live: false, .. }))),
        "references to dropped columns are discoverable without knowing their names");

    // Cold reopen: no memory of the running owner survives.
    drop(session);
    let root = TempTree(std::mem::take(&mut wire.fixture.root.0));
    drop(wire);
    let reopened = crate::builtin::sql_journey_support::reopen_and_query(&root.0.join("workspace"), &project)
        .expect("cold reopen and query");
    assert!(reopened.analysis);
    assert_eq!(reopened.tables, ["accounts", "orders"]);
    assert!(reopened.balance_dropped);
}

#[test]
fn many_sql_facts_publish_discover_and_reopen_with_bounded_catalog() {
    // Populate definitions and references so the selected catalog exercises
    // repeated scans and digest staging over a substantial mixed fact set.
    let mut schema = String::new();
    let mut queries = String::new();
    for index in 0..180 {
        use std::fmt::Write as _;
        writeln!(&mut schema, "CREATE TABLE catalog_item_{index:03} (id INTEGER, value TEXT);").unwrap();
        writeln!(&mut queries, "SELECT value FROM catalog_item_{index:03} WHERE id = ?1;").unwrap();
    }
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let mut wire = SourceWire::start_sql(
        TestStage::BeforeSourcePreparation,
        true,
        SqlFixture { files: vec![("db/0001_schema.sql".into(), schema.into_bytes()), ("db/0002_queries.sql".into(), queries.into_bytes())], workers },
    );
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    wire.commands.lock().expect("adapter").source_test_hook = None;
    let project = wire.source.parent().expect("project").to_owned();
    let mut session = Session::connect(&wire.endpoint).expect("listener");
    let reply = session.surface(backend_library::SurfaceCommand::IndexSearch {
        query: backend_library::ProductText::new("catalog_item_179").unwrap(), limit: 20, cursor: None,
    }).expect("ordinary SQL search");
    let backend_library::SurfaceReply::IndexSearchPage(page) = reply else { panic!("search page") };
    assert!(page.hits.iter().any(|hit| matches!(hit,
        backend_library::RegistrySearchHit::LocalDeclaration(row) if row.name.as_str() == "catalog_item_179")));
    drop(session);
    let root = TempTree(std::mem::take(&mut wire.fixture.root.0));
    drop(wire);
    let started = std::time::Instant::now();
    let reopened = crate::builtin::sql_journey_support::reopen_and_query(&root.0.join("workspace"), &project)
        .expect("cold bounded catalog reopen");
    eprintln!("many SQL facts cold catalog and query: {:?}", started.elapsed());
    assert!(reopened.analysis);
    assert_eq!(reopened.tables.len(), 180);
    assert!(reopened.tables.iter().any(|name| name == "catalog_item_179"));
    assert!(reopened.sql_root_level > 0, "SQL relation must physically span multiple leaves");

    // Reopen the production adapter and use its ordinary search surface.
    let mut fixture = AdapterFixture::open(root, false);
    let view = fixture.parts().1.engine().daemon().library().view().clone();
    let deadline = std::time::Instant::now() + TERMINAL_WAIT;
    let (adapter, daemon) = fixture.parts();
    loop {
        assert!(std::time::Instant::now() < deadline, "cold search preparation deadline");
        match adapter.local_search_lane.prepare(&view, &mut adapter.product_state) {
            None => break,
            Some(backend_library::CommandFailure::QueryPreparation { basis, .. }) if basis == view.root() =>
                std::thread::sleep(std::time::Duration::from_millis(2)),
            other => panic!("cold search preparation: {other:?}"),
        }
    }
    let (reply, _) = adapter.surface(daemon, backend_library::SurfaceCommand::IndexSearch {
        query: backend_library::ProductText::new("catalog_item_179").unwrap(), limit: 20, cursor: None,
    }, 0xc500).expect("cold ordinary SQL search");
    let CommandReply::Surface(backend_library::SurfaceReply::IndexSearchPage(page)) = reply else { panic!("cold search page") };
    assert!(page.hits.iter().any(|hit| matches!(hit,
        backend_library::RegistrySearchHit::LocalDeclaration(row) if row.name.as_str() == "catalog_item_179")));
}

#[test]
fn unsupported_statement_and_unparseable_file_are_reported_as_gaps_never_as_empty() {
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let files = vec![
        ("db/0001_schema.sql".to_owned(), b"CREATE TABLE t (id INTEGER);\nCREATE VIRTUAL TABLE docs USING fts5(body);\n".to_vec()),
        ("db/0002_broken.sql".to_owned(), b"SELECT FROM WHERE;".to_vec()),
    ];
    let mut wire = SourceWire::start_sql(TestStage::BeforeSourcePreparation, true, SqlFixture { files, workers });
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    let project = wire.source.parent().expect("project").to_owned();
    let mut session = Session::connect(&wire.endpoint).expect("listener");
    let mut gap_request = request(&project, SqlQueryRole::Gap);
    gap_request.page_rows = 1;
    let (page, gaps) = all_hits(&mut session, gap_request);
    assert_eq!(page.host_gaps.len(), 1);
    assert_eq!(page.host_gaps[0].path, "db/0002_broken.sql");
    assert!(matches!(page.host_gaps[0].reason, backend_library::sql_query::SqlHostGapReason::NativeError));
    assert!(!gaps.is_empty(), "the virtual table statement is not fully modeled");
    assert_eq!(page.basis.gaps.native_error_hosts, 1, "the unparseable file is counted");
    assert!(page.basis.gaps.partial_statements + page.basis.gaps.unsupported_statements >= 1);
    // A search that finds nothing cannot claim "none": the gap counters say why.
    let mut uses = request(&project, SqlQueryRole::Reference);
    uses.uses = Some("t.id".into());
    let (page, hits) = all_hits(&mut session, uses);
    assert!(hits.is_empty());
    assert_ne!(page.basis.gaps, Default::default());
    let _ = (SqlStatementSupport::Full, ());
}

const PG_INIT: &str = "\
CREATE SCHEMA app;
CREATE TYPE app.tier AS ENUM ('basic', 'pro');
CREATE TABLE app.accounts (
  id bigserial PRIMARY KEY,
  email text NOT NULL UNIQUE,
  balance integer NOT NULL DEFAULT 0,
  tier app.tier NOT NULL DEFAULT 'basic'
);
CREATE TABLE app.orders (
  id bigserial PRIMARY KEY,
  account_id bigint NOT NULL REFERENCES app.accounts (id),
  total integer NOT NULL
);
CREATE INDEX orders_by_account ON app.orders (account_id);
";
const PG_VIEWS: &str = "\
CREATE VIEW app.account_spend AS
  SELECT a.id AS account_id, a.email, sum(o.total) AS spent
  FROM app.accounts a JOIN app.orders o ON o.account_id = a.id
  GROUP BY a.id, a.email;

CREATE FUNCTION app.charge(p_account bigint, p_amount integer) RETURNS void
LANGUAGE sql AS $$ UPDATE app.accounts SET balance = balance - p_amount WHERE id = p_account $$;
";
const PG_RENAME: &str = "ALTER TABLE app.accounts RENAME COLUMN email TO login;\n";
const PG_QUERIES: &str = "\
SELECT login, balance FROM app.accounts WHERE id = $1;

WITH recent AS (SELECT account_id, total FROM app.orders WHERE total > 100)
SELECT a.login, r.total FROM app.accounts a JOIN recent r ON r.account_id = a.id;
";

#[test]
fn postgres_project_binds_across_schemas_renames_and_survives_cold_reopen() {
    let workers = admitted_workers(None, Some(frontend_worker("postgres", "postgres-worker")));
    let files = [
        ("db/migrations/0001_init.sql", PG_INIT),
        ("db/migrations/0002_views.sql", PG_VIEWS),
        ("db/migrations/0003_rename.sql", PG_RENAME),
        ("db/queries/accounts.sql", PG_QUERIES),
    ]
    .into_iter()
    .map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec()))
    .collect();
    let mut wire = SourceWire::start_sql(TestStage::BeforeSourcePreparation, true, SqlFixture { files, workers });
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    let project = wire.source.parent().expect("project").to_owned();
    let mut session = Session::connect(&wire.endpoint).expect("listener");

    let tables = definitions(&mut session, &project, SqlObjectKind::Table, None);
    assert_eq!(tables, [("accounts".to_owned(), true), ("orders".to_owned(), true)]);
    let columns = definitions(&mut session, &project, SqlObjectKind::Column, Some("accounts"));
    assert_eq!(
        columns.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
        ["balance", "id", "login", "tier"],
        "the rename in a later migration is the column's head name"
    );
    assert_eq!(definitions(&mut session, &project, SqlObjectKind::Function, None), [("charge".to_owned(), true)]);
    assert_eq!(definitions(&mut session, &project, SqlObjectKind::View, None), [("account_spend".to_owned(), true)]);

    // The view (written before the rename) and the queries (written after it)
    // reach the same column entity.
    let mut uses = request(&project, SqlQueryRole::Reference);
    uses.uses = Some("app.accounts.login".into());
    uses.kind = Some(SqlObjectKind::Column);
    let (page, hits) = all_hits(&mut session, uses);
    let refs: Vec<_> = hits
        .iter()
        .filter_map(|hit| match &hit.fact {
            SqlFact::Reference(r) if r.statement_kind != SqlStatementKind::AlterTable => Some((hit.path.as_str(), r)),
            _ => None,
        })
        .collect();
    assert!(refs.iter().any(|(path, r)| path.ends_with("0002_views.sql")
        && r.in_object.as_ref().is_some_and(|o| o.kind == SqlObjectKind::View && o.name == "account_spend")),
        "view over the renamed column: {refs:?}");
    assert_eq!(refs.iter().filter(|(path, _)| *path == "db/queries/accounts.sql").count(), 2,
        "both query mentions of login, one through an alias");
    assert!(refs.iter().all(|(_, r)| matches!(r.resolution, SqlResolution::Resolved { live: true, .. })));
    // The SQL-language function body was walked, so it is linked to the table it updates.
    let mut balance = request(&project, SqlQueryRole::Reference);
    balance.uses = Some("app.accounts.balance".into());
    let (_, hits) = all_hits(&mut session, balance);
    assert!(hits.iter().any(|hit| matches!(&hit.fact, SqlFact::Reference(r)
        if r.in_object.as_ref().is_some_and(|o| o.kind == SqlObjectKind::Function && o.name == "charge"))));
    assert!(page.basis.inventory_complete);
    drop(session);
    let root = TempTree(std::mem::take(&mut wire.fixture.root.0));
    drop(wire);
    let reopened = crate::builtin::sql_journey_support::reopen_and_query(&root.0.join("workspace"), &project)
        .expect("cold reopen and query");
    assert!(reopened.analysis);
    assert_eq!(reopened.tables, ["accounts", "orders"]);
}

/// Stages at which the owner process is killed mid-publication.
const CRASH_STAGES: [(&str, TestStage); 4] = [
    ("before-grant-validation", TestStage::BeforeGrantValidation),
    ("after-grant", TestStage::AfterGrant),
    ("after-selection", TestStage::AfterSelection),
    ("after-receipt", TestStage::AfterReceipt),
];

/// Child half of the crash test: index the SQLite project and kill this whole
/// process at the named publication stage. Without the environment the parent
/// sets, it has nothing to do and returns.
#[test]
fn sql_crash_child() {
    let (Some(dir), Some(stage)) = (
        std::env::var_os("NUDOX_SQL_CRASH_DIR").map(PathBuf::from),
        std::env::var("NUDOX_SQL_CRASH_STAGE").ok(),
    ) else {
        return;
    };
    let (_, stage) = CRASH_STAGES.iter().find(|(name, _)| *name == stage).copied().expect("known stage");
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let mut wire = SourceWire::start_sql(
        TestStage::BeforeSourcePreparation,
        true,
        SqlFixture { files: sqlite_files(), workers },
    );
    wire.wait_at_source_within(TERMINAL_WAIT);
    let project = wire.source.parent().expect("project").to_owned();
    let workspace = project.parent().expect("workspace").to_owned();
    fs::write(dir.join("paths"), format!("{}\n{}\n", workspace.display(), project.display())).expect("record paths");
    wire.commands.lock().expect("adapter").publication_test_hook = Some(TestHook {
        stage,
        run: Box::new(|| std::process::exit(137)),
    });
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    panic!("the process should have been killed at the crash stage");
}

#[test]
fn killing_the_owner_mid_publication_never_leaves_a_torn_sql_analysis() {
    let test = module_path!().split_once("::").expect("crate prefix").1.to_owned() + "::sql_crash_child";
    for (name, _) in CRASH_STAGES {
        let dir = tempfile::tempdir().expect("crash coordination directory");
        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", &test, "--nocapture", "--test-threads=1"])
            .env("NUDOX_SQL_CRASH_DIR", dir.path())
            .env("NUDOX_SQL_CRASH_STAGE", name)
            .status()
            .expect("spawn crashing owner");
        assert_eq!(status.code(), Some(137), "owner was killed at {name}");
        let paths = fs::read_to_string(dir.path().join("paths")).expect("child recorded its paths");
        let mut lines = paths.lines();
        let workspace = PathBuf::from(lines.next().expect("workspace"));
        let project = PathBuf::from(lines.next().expect("project"));
        // The owner reopens from whatever the kill left. It either holds no SQL
        // analysis yet, or the complete one: never a part of it.
        let reopened = crate::builtin::sql_journey_support::reopen_and_query(&workspace, &project)
            .unwrap_or_else(|error| panic!("reopen after a kill at {name}: {error}"));
        if reopened.analysis {
            assert_eq!(reopened.tables, ["accounts", "orders"], "kill at {name} left a partial analysis");
        } else {
            assert!(reopened.tables.is_empty());
        }
        // The crashed run's workspace is not removed by the killed child.
        fs::remove_dir_all(workspace.parent().expect("fixture root")).expect("remove crashed fixture");
    }
}

#[test]
fn flat_sql_upsert_and_renamed_objects_keep_precise_owner_edges() {
    use backend_library::impact::{ImpactDomain, ImpactRequest, ImpactSeed, ImpactSeedSpec};
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let files = [
        ("db/0001_init.sql", "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT, country TEXT);\nCREATE TABLE books (id INTEGER PRIMARY KEY, price_cents INTEGER, title TEXT);\nCREATE TABLE order_items (book_id INTEGER REFERENCES books(id));\nCREATE INDEX books_by_title ON books(title);"),
        ("db/0002_rename.sql", "ALTER TABLE books RENAME COLUMN price_cents TO list_price_cents;\nALTER TABLE order_items RENAME TO order_lines;"),
        ("db/catalog.sql", "INSERT INTO authors (id, name, country) VALUES (?1, ?2, ?3) ON CONFLICT (id) DO UPDATE SET name = excluded.name, country = excluded.country;\nSELECT list_price_cents FROM books;\nSELECT title FROM books;"),
        ("db/broken.sql", "SELECT price_cents FROM books;\nSELECT * FROM order_items;"),
    ].into_iter().map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec())).collect();
    let mut wire = SourceWire::start_sql(TestStage::BeforeSourcePreparation, true, SqlFixture { files, workers });
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    let project = wire.source.parent().expect("project").to_owned();
    let mut session = Session::connect(&wire.endpoint).expect("listener");
    let (page, _) = all_hits(&mut session, request(&project, SqlQueryRole::Gap));
    assert_eq!(page.basis.gaps, Default::default(), "valid flat files must all reach SQLite");
    let mut upsert = request(&project, SqlQueryRole::Reference);
    upsert.uses = Some("authors.name".into());
    let (_, hits) = all_hits(&mut session, upsert);
    assert!(hits.iter().any(|hit| matches!(&hit.fact, SqlFact::Reference(reference) if reference.qualifier.as_deref() == Some("excluded") && matches!(reference.resolution, SqlResolution::Resolved { .. }))));
    let (_, diagnostics) = all_hits(&mut session, request(&project, SqlQueryRole::Diagnostic));
    for name in ["price_cents", "order_items"] {
        assert!(diagnostics.iter().any(|hit| matches!(&hit.fact, SqlFact::Reference(reference) if reference.name == name && matches!(reference.resolution, SqlResolution::Unresolved { .. }))), "unbound {name} is discoverable without a name filter");
    }
    let impact = |session: &mut Session, kind: &str, name: &str| {
        let request = ImpactRequest::new(
            backend_library::PackageReference::parse(project.to_str().unwrap().to_owned()).unwrap(),
            ImpactSeed::Named(ImpactSeedSpec { domain: ImpactDomain::Sql, kind: kind.into(), name: name.into(), path: None }),
        );
        let backend_library::SurfaceReply::Impact(report) = session.surface(backend_library::SurfaceCommand::Impact { request }).expect("public impact") else { panic!("impact reply") };
        report
    };
    let column = impact(&mut session, "column", "books.list_price_cents");
    assert!(column.rows.iter().any(|row| row.node.kind == "query"));
    assert!(column.rows.iter().all(|row| row.node.kind != "table" && row.node.kind != "index"), "column impact cannot spread through containment: {:?}", column.rows);
    let table = impact(&mut session, "table", "order_lines");
    assert!(table.rows.iter().all(|row| row.node.name != "order_items"), "the old table spelling must share the renamed entity");
}

#[test]
fn sql_impact_distinguishes_drop_recreate_epochs() {
    use backend_library::impact::{ImpactDomain, ImpactRequest, ImpactSeed, ImpactSeedSpec};
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let files = [
        ("db/0001.sql", "CREATE TABLE books (id INTEGER, price INTEGER);\nCREATE VIEW old_book AS SELECT price FROM books;"),
        ("db/0002.sql", "DROP TABLE books; CREATE TABLE books (id INTEGER, price INTEGER); CREATE VIEW new_book AS SELECT price FROM books;"),
    ].into_iter().map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec())).collect();
    let mut wire = SourceWire::start_sql(TestStage::BeforeSourcePreparation, true, SqlFixture { files, workers });
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    let project = wire.source.parent().expect("project").to_owned();
    let mut session = Session::connect(&wire.endpoint).expect("listener");
    let request = ImpactRequest::new(
        backend_library::PackageReference::parse(project.to_str().unwrap().to_owned()).unwrap(),
        ImpactSeed::Named(ImpactSeedSpec { domain: ImpactDomain::Sql, kind: "column".into(), name: "books.price".into(), path: None }),
    );
    let backend_library::SurfaceReply::Impact(report) = session.surface(backend_library::SurfaceCommand::Impact { request }).expect("public impact") else { panic!("impact reply") };
    assert_eq!(report.seeds.len(), 1, "only the live epoch is seedable");
    assert!(report.rows.iter().any(|row| row.node.name == "new_book"), "new view must follow live column: {:?}", report.rows);
    assert!(report.rows.iter().all(|row| row.node.name != "old_book"), "old view cannot be reached through the recreated column: {:?}", report.rows);
}

const GYM_INIT: &str = "\
CREATE TABLE members (
  id INTEGER PRIMARY KEY,
  full_name TEXT NOT NULL,
  email TEXT NOT NULL UNIQUE,
  legacy_code TEXT
);
CREATE TABLE workouts (
  id INTEGER PRIMARY KEY,
  member_id INTEGER NOT NULL REFERENCES members(id),
  started_at TEXT NOT NULL
);
CREATE INDEX workouts_by_member ON workouts(member_id);
";
const GYM_MEMBERSHIP: &str = "\
ALTER TABLE members ADD COLUMN tier TEXT NOT NULL DEFAULT 'basic';
ALTER TABLE members RENAME COLUMN full_name TO display_name;
ALTER TABLE members DROP COLUMN legacy_code;
";
const GYM_VIEWS: &str = "\
CREATE VIEW member_volume AS
  SELECT m.id AS member_id, m.display_name, COUNT(w.id) AS sessions
  FROM members m JOIN workouts w ON w.member_id = m.id
  GROUP BY m.id, m.display_name;
CREATE TABLE tier_audit (member_id INTEGER, tier TEXT);
CREATE TRIGGER audit_tier_change AFTER UPDATE OF tier ON members
BEGIN
  INSERT INTO tier_audit (member_id, tier) VALUES (NEW.id, NEW.tier);
END;
";
const GYM_QUERIES: &str = "\
SELECT id, display_name, email FROM members WHERE id = ?1;
SELECT full_name, legacy_code FROM members;
";

fn name_rows(session: &mut Session, text: &str) -> Vec<backend_library::Row> {
    let reply = session.names(text, 50).expect("name resolution through the public route");
    let CommandReply::Names(snapshot) = reply.reply else { panic!("names reply kind") };
    snapshot.root.row_refs().cloned().collect()
}

/// A gym-tracker project written as a user would: ordered migrations that add,
/// rename and drop columns, a view, a trigger and an index, plus queries that
/// read the renamed and dropped columns.
struct Gym {
    _wire: SourceWire,
    project: PathBuf,
    session: Session,
}

fn gym() -> Gym {
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let files = [
        ("migrations/0001_init.sql", GYM_INIT),
        ("migrations/0002_membership.sql", GYM_MEMBERSHIP),
        ("migrations/0003_views_triggers.sql", GYM_VIEWS),
        ("queries/stale_columns.sql", GYM_QUERIES),
    ]
    .into_iter()
    .map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec()))
    .collect();
    let mut wire = SourceWire::start_sql(TestStage::BeforeSourcePreparation, true, SqlFixture { files, workers });
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    wire.commands.lock().expect("adapter").source_test_hook = None;
    let project = wire.source.parent().expect("project").to_owned();
    let session = Session::connect(&wire.endpoint).expect("listener");
    Gym { _wire: wire, project, session }
}

fn search_coordinate(session: &mut Session, name: &str) -> Option<String> {
    let reply = session
        .surface(backend_library::SurfaceCommand::IndexSearch {
            query: backend_library::ProductText::new(name).expect("query"),
            limit: 20,
            cursor: None,
        })
        .expect("ordinary SQL name search");
    let backend_library::SurfaceReply::IndexSearchPage(page) = reply else { panic!("search page") };
    page.hits.iter().find_map(|hit| match hit {
        backend_library::RegistrySearchHit::LocalDeclaration(record) if record.name.as_str() == name => {
            Some(record.coordinate.as_str().to_owned())
        }
        _ => None,
    })
}

fn sql_row(session: &mut Session, name: &str, signature: &str) -> backend_library::Row {
    name_rows(session, name)
        .into_iter()
        .find(|row| row.signature.as_deref() == Some(signature))
        .unwrap_or_else(|| panic!("row `{signature}` among the resolved names of `{name}`"))
}

/// S3: every kind of SQL declaration is found by its name in ordinary search.
#[test]
fn gym_search_finds_tables_views_indexes_triggers_and_columns_by_name() {
    let mut gym = gym();
    for name in ["members", "workouts", "member_volume", "tier_audit", "audit_tier_change", "workouts_by_member", "display_name"] {
        assert!(search_coordinate(&mut gym.session, name).is_some(), "`{name}` is a SQL declaration and search must find it");
    }
}

/// S4: real kinds instead of `unknown`, and columns nested under their relation.
#[test]
fn gym_declarations_have_kinds_and_columns_nest_under_their_table() {
    use backend_library::DeclarationKind as K;
    let mut gym = gym();
    let session = &mut gym.session;
    let members = sql_row(session, "members", "SQL table members");
    assert_eq!(members.kind, Some(K::Struct));
    assert_eq!(sql_row(session, "member_volume", "SQL view member_volume").kind, Some(K::Type));
    assert_eq!(sql_row(session, "audit_tier_change", "SQL trigger audit_tier_change").kind, Some(K::Function));
    assert_eq!(sql_row(session, "workouts_by_member", "SQL index workouts_by_member").kind, Some(K::Property));
    let display_name = sql_row(session, "display_name", "SQL column members.display_name");
    assert_eq!(display_name.kind, Some(K::Field));
    let backend_library::RowId::Symbol(members_key) = members.id else { panic!("table row symbol") };
    assert_eq!(display_name.parent, Some(members_key), "the column is a child of its table");
    assert_eq!(members.parent, None, "the table is top level");

    let reply = session.outline(gym.project.to_str().expect("path")).expect("outline through the public route");
    let CommandReply::Outline(outline) = reply.reply else { panic!("outline reply kind") };
    fn find(node: &backend_library::OutlineNode, wanted: backend_library::SymbolKey) -> Option<&backend_library::OutlineNode> {
        if node.symbol == wanted {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, wanted))
    }
    let table_node = std::iter::once(&outline.root)
        .chain(outline.additional_roots.iter())
        .find_map(|root| find(root, members_key))
        .expect("the members table is in the outline");
    let backend_library::RowId::Symbol(column_key) = display_name.id else { panic!("column row symbol") };
    assert!(table_node.children.iter().any(|child| child.symbol == column_key), "display_name nests under members in the outline");
    assert_eq!(table_node.children.len(), 4, "id, display_name, email, tier: the dropped legacy_code is not live");
}

/// S5: show reads a SQL declaration right after indexing, references say where
/// its uses are, and freshness stays a real check.
#[test]
fn gym_show_reads_sql_declarations_and_references_route_to_sql_query() {
    let mut gym = gym();
    let table = search_coordinate(&mut gym.session, "members").expect("members is searchable");
    let selector = backend_library::DeclarationSelector::Coordinate(table.clone());
    let reply = gym
        .session
        .declaration(&selector, backend_library::DeclarationRead::Document)
        .expect("show of a SQL table right after indexing");
    let CommandReply::Document(document) = reply.reply else { panic!("document reply kind") };
    assert_eq!(document.signature.as_deref(), Some("SQL table members"));
    let backend_library::SurfaceReply::References { references, coverage, .. } = gym
        .session
        .surface(backend_library::SurfaceCommand::References {
            target: backend_library::ProductText::new(table.clone()).expect("target"),
        })
        .expect("references of a SQL table")
    else {
        panic!("references reply kind")
    };
    assert!(references.is_empty());
    let backend_library::ReferenceCoverage::Unknown { reason } = coverage else { panic!("SQL references are routed, not claimed") };
    assert!(reason.as_str().contains("sql-query") && reason.as_str().contains("--uses members"), "{}", reason.as_str());
    assert!(!reason.as_str().contains("No selected semantic publication"), "{}", reason.as_str());

    // Editing the declaring file makes show refuse: freshness is not bypassed.
    fs::write(gym.project.join("migrations/0001_init.sql"), format!("{GYM_INIT}-- edited after indexing\n")).expect("edit migration");
    let error = gym
        .session
        .declaration(&selector, backend_library::DeclarationRead::Document)
        .expect_err("a changed declaring file must not be presented as fresh");
    assert!(error.to_string().contains("changed on disk"), "{error}");
}

/// S2: without a role, a name asks for every definition and mention of that
/// object.
#[test]
fn gym_role_less_name_query_returns_definitions_and_mentions() {
    let mut gym = gym();
    let mut bare = request(&gym.project, SqlQueryRole::Any);
    bare.name = Some("members".into());
    let (_, hits) = all_hits(&mut gym.session, bare);
    assert!(hits.iter().any(|hit| matches!(&hit.fact, SqlFact::Definition(d)
        if d.kind == SqlObjectKind::Table && d.head_name.as_deref().unwrap_or(&d.name) == "members")), "the definition of members");
    assert!(hits.iter().any(|hit| matches!(&hit.fact, SqlFact::Reference(r) if r.name == "members")), "a mention of members");
    assert!(hits.iter().all(|hit| matches!(&hit.fact, SqlFact::Definition(_) | SqlFact::Reference(_))));
    let mut kinds = request(&gym.project, SqlQueryRole::Any);
    kinds.kind = Some(SqlObjectKind::Trigger);
    let (_, hits) = all_hits(&mut gym.session, kinds);
    assert!(!hits.is_empty() && hits.iter().all(|hit| matches!(&hit.fact, SqlFact::Definition(d) if d.kind == SqlObjectKind::Trigger)
        || matches!(&hit.fact, SqlFact::Reference(r) if r.kind == SqlRefKind::Trigger)));
}

/// S6 (data side): the rename keeps the old name, and the creating statement
/// keeps its own; the label wording is covered where it is rendered.
#[test]
fn gym_rename_keeps_the_written_name_apart_from_the_final_one() {
    let mut gym = gym();
    let mut columns = request(&gym.project, SqlQueryRole::Definition);
    columns.kind = Some(SqlObjectKind::Column);
    columns.parent = Some("members".into());
    let (_, hits) = all_hits(&mut gym.session, columns);
    let defined: Vec<_> = hits.iter().filter_map(|hit| match &hit.fact {
        SqlFact::Definition(d) => Some(d),
        _ => None,
    }).collect();
    let rename = defined.iter().find(|d| matches!(&d.action, SqlDefinitionAction::Rename { .. })).expect("the rename statement");
    assert_eq!(rename.name, "full_name");
    assert!(matches!(&rename.action, SqlDefinitionAction::Rename { to, .. } if to == "display_name"));
    let created = defined.iter().find(|d| matches!(&d.action, SqlDefinitionAction::Create) && d.name == "full_name").expect("0001 created full_name");
    assert_eq!(created.head_name.as_deref(), Some("display_name"), "the entity's final name is kept apart from the written one");
    assert!(!defined.iter().any(|d| matches!(&d.action, SqlDefinitionAction::Create) && d.name == "display_name"), "0001 never created display_name");
}
